//! v1.135.235: обработка микрофона для звонков без наушников (чистый Rust, rustfft).
//!
//! `Aec` — акустический эхоподавитель: PBFDAF (partitioned-block frequency-domain NLMS,
//! overlap-save), 24 партиции × 20 мс ≈ 480 мс хвоста (покрывает задержку вывод→микрофон
//! Windows/Android + реверберацию комнаты), контроль двойного разговора по отношению
//! энергий ошибка/микрофон, подавление остаточного эха (спектральный Винер по оценке эха)
//! и энергетическая страховка на время сходимости фильтра.
//!
//! `Agc` — автоусиление: цель −18 dBFS, 0.25…20×, быстрая атака / медленный подъём,
//! следящий шумовой порог (тишину и шум не раздуваем), мягкий лимитер.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

/// Блок = один Opus-кадр (20 мс @ 48 кГц).
pub(crate) const N: usize = 960;
const M: usize = 2 * N;
/// Партиций фильтра (хвост = P × 20 мс).
const P: usize = 24;
/// RMS дальнего конца, ниже которого считаем, что в динамиках тишина.
const FAR_FLOOR: f32 = 1e-4;

fn energy(v: &[f32]) -> f32 {
    v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32
}

pub(crate) struct Aec {
    fft: Arc<dyn Fft<f32>>,
    ifft: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex<f32>>,
    buf: Vec<Complex<f32>>,
    buf2: Vec<Complex<f32>>,
    /// кольцо спектров дальнего конца; самый свежий — `x_spec[x_pos]`
    x_spec: Vec<Vec<Complex<f32>>>,
    x_pos: usize,
    px: Vec<f32>,
    w: Vec<Vec<Complex<f32>>>,
    x_prev: Vec<f32>,
    e_prev: Vec<f32>,
    y_prev: Vec<f32>,
    y: Vec<f32>,
    e: Vec<f32>,
    gain: Vec<f32>,
    gain_sm: Vec<f32>,
    /// блоков с момента последней активности дальнего конца
    far_idle: usize,
    /// огибающая дальнего конца (быстрая атака, медленный спад) — для страховки
    far_env: f32,
    constrain_at: usize,
    diverge: usize,
    /// блоков адаптации с начала звонка (страховка действует только в начале)
    far_blocks: u32,
    /// сглаженное подавление эха, дБ (диагностика + признак сходимости)
    pub(crate) erle_db: f32,
}

impl Aec {
    pub(crate) fn new() -> Self {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(M);
        let ifft = planner.plan_fft_inverse(M);
        let scratch_len = fft.get_inplace_scratch_len().max(ifft.get_inplace_scratch_len());
        let zero = Complex::new(0.0, 0.0);
        Self {
            fft,
            ifft,
            scratch: vec![zero; scratch_len],
            buf: vec![zero; M],
            buf2: vec![zero; M],
            x_spec: vec![vec![zero; M]; P],
            x_pos: 0,
            px: vec![0.0; M],
            w: vec![vec![zero; M]; P],
            x_prev: vec![0.0; N],
            e_prev: vec![0.0; N],
            y_prev: vec![0.0; N],
            y: vec![0.0; N],
            e: vec![0.0; N],
            gain: vec![1.0; M],
            gain_sm: vec![1.0; M],
            far_idle: P + 100,
            far_env: 0.0,
            constrain_at: 0,
            diverge: 0,
            far_blocks: 0,
            erle_db: 0.0,
        }
    }

    /// Дальний конец звучал в последние ~0.5 с (в динамиках не тишина).
    pub(crate) fn far_active(&self) -> bool {
        self.far_idle <= P
    }

    fn reset_filter(&mut self) {
        for w in self.w.iter_mut() {
            w.iter_mut().for_each(|c| *c = Complex::new(0.0, 0.0));
        }
        self.erle_db = 0.0;
    }

    /// `d` — кадр микрофона (N, перезаписывается результатом), `x` — что в этот момент
    /// играли в динамики (N, 48 кГц моно, тот же сигнал, что ушёл в вывод).
    pub(crate) fn process(&mut self, d: &mut [f32], x: &[f32]) {
        debug_assert!(d.len() == N && x.len() == N);
        let x_rms = energy(x).sqrt();
        if x_rms > FAR_FLOOR {
            self.far_idle = 0;
        } else {
            self.far_idle += 1;
        }
        self.far_env = x_rms.max(self.far_env * 0.9);

        // 1. спектр блока дальнего конца [x_prev | x] → кольцо
        for i in 0..N {
            self.buf[i] = Complex::new(self.x_prev[i], 0.0);
            self.buf[N + i] = Complex::new(x[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        self.x_pos = (self.x_pos + P - 1) % P;
        self.x_spec[self.x_pos].copy_from_slice(&self.buf);
        self.x_prev.copy_from_slice(x);

        // 2. оценка эха: Y = Σ W_p ⊙ X_{k−p}; попутно мощность дальнего конца по бинам
        self.buf2.iter_mut().for_each(|c| *c = Complex::new(0.0, 0.0));
        self.px.iter_mut().for_each(|p| *p = 0.0);
        for p in 0..P {
            let xs = &self.x_spec[(self.x_pos + p) % P];
            let ws = &self.w[p];
            for k in 0..M {
                self.buf2[k] += ws[k] * xs[k];
                self.px[k] += xs[k].norm_sqr();
            }
        }
        self.ifft.process_with_scratch(&mut self.buf2, &mut self.scratch);
        let inv_m = 1.0 / M as f32;
        for i in 0..N {
            self.y[i] = self.buf2[N + i].re * inv_m;
            self.e[i] = d[i] - self.y[i];
        }

        let d_pow = energy(d);
        let e_pow = energy(&self.e);
        let far_active = self.far_idle <= P;

        // 3. адаптация (NLMS в частотной области) — только пока дальний конец звучит
        if far_active && d_pow > 1e-8 {
            let r = e_pow / d_pow;
            if r > 1.5 {
                self.diverge += 1;
                if self.diverge > 25 {
                    self.reset_filter();
                    self.diverge = 0;
                }
            } else {
                self.diverge = 0;
            }
            // ERLE меряем только на «чистом» эхе (ошибка ≪ микрофон); при двойном разговоре —
            // медленно забываем, чтобы при смене эхо-пути подавитель снова стал жёстче
            if r < 0.25 {
                let erle = 10.0 * (d_pow / e_pow.max(1e-12)).log10();
                self.erle_db = self.erle_db * 0.9 + erle.min(40.0) * 0.1;
            } else if r > 0.8 {
                self.erle_db *= 0.98;
            }
            self.far_blocks = self.far_blocks.saturating_add(1);
            // двойной разговор: ошибка ≈ микрофон → шаг малый; ошибка ≫ микрофон → стоп
            let mu = if r < 0.3 {
                0.5
            } else if r < 0.7 {
                0.3
            } else if r <= 1.05 {
                0.08
            } else {
                0.0
            };
            if mu > 0.0 {
                for i in 0..N {
                    self.buf[i] = Complex::new(0.0, 0.0);
                    self.buf[N + i] = Complex::new(self.e[i], 0.0);
                }
                self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
                let delta = self.px.iter().sum::<f32>() * inv_m * 0.01 + 1e-6;
                for p in 0..P {
                    let xs = &self.x_spec[(self.x_pos + p) % P];
                    for k in 0..M {
                        self.buf2[k] = xs[k].conj() * self.buf[k] * (mu / (self.px[k] + delta));
                    }
                    if p == self.constrain_at {
                        // ограничение градиента (линейная свёртка) — по одной партиции за блок
                        self.ifft.process_with_scratch(&mut self.buf2, &mut self.scratch);
                        for k in 0..N {
                            self.buf2[k] *= inv_m;
                        }
                        for k in N..M {
                            self.buf2[k] = Complex::new(0.0, 0.0);
                        }
                        self.fft.process_with_scratch(&mut self.buf2, &mut self.scratch);
                    }
                    let ws = &mut self.w[p];
                    for k in 0..M {
                        ws[k] += self.buf2[k];
                    }
                }
                self.constrain_at = (self.constrain_at + 1) % P;
            }
        }

        // 4. остаточное эхо: спектральный Винер по оценке эха Y (пока дальний конец звучал недавно)
        if self.far_idle <= P + 5 {
            for i in 0..N {
                self.buf[i] = Complex::new(self.e_prev[i], 0.0);
                self.buf[N + i] = Complex::new(self.e[i], 0.0);
                self.buf2[i] = Complex::new(self.y_prev[i], 0.0);
                self.buf2[N + i] = Complex::new(self.y[i], 0.0);
            }
            self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
            self.fft.process_with_scratch(&mut self.buf2, &mut self.scratch);
            // остаточное эхо ≈ оценка эха × утечка (10^(−ERLE/10)): пока фильтр не сошёлся —
            // давим почти всё, что похоже на эхо; сошёлся — ближний голос почти не трогаем
            const BETA: f32 = 2.0;
            const FLOOR: f32 = 0.03;
            let leak = 10f32.powf(-self.erle_db / 10.0).clamp(0.01, 1.0);
            for k in 0..M {
                let pe = self.buf[k].norm_sqr();
                let py = self.buf2[k].norm_sqr() * leak;
                let g = (pe / (pe + BETA * py + 1e-12)).max(FLOOR);
                self.gain[k] = self.gain[k] * 0.4 + g * 0.6;
            }
            for k in 0..M {
                let l = self.gain[(k + M - 1) % M];
                let r = self.gain[(k + 1) % M];
                self.gain_sm[k] = (l + 2.0 * self.gain[k] + r) * 0.25;
            }
            for k in 0..M {
                self.buf[k] *= self.gain_sm[k];
            }
            self.ifft.process_with_scratch(&mut self.buf, &mut self.scratch);
            for i in 0..N {
                d[i] = self.buf[N + i].re * inv_m;
            }
            // страховка на первые ~3 с звучания дальнего конца: фильтр ещё не научился, а из
            // динамиков громко и микрофон не громче дальнего конца — это наше же эхо → −20 дБ
            if self.far_blocks < 150 && self.erle_db < 6.0 && self.far_env > 0.02 && d_pow.sqrt() < self.far_env * 1.5 {
                d.iter_mut().for_each(|s| *s *= 0.1);
            }
        } else {
            d.copy_from_slice(&self.e);
            self.gain.iter_mut().for_each(|g| *g = 1.0);
        }
        self.e_prev.copy_from_slice(&self.e);
        self.y_prev.copy_from_slice(&self.y);
    }
}

/// Автоусиление микрофона.
pub(crate) struct Agc {
    gain: f32,
    floor: f32,
}

pub(crate) fn soft_clip(s: f32) -> f32 {
    let a = s.abs();
    if a <= 0.8 {
        s
    } else {
        let over = a - 0.8;
        (0.8 + over / (1.0 + over * 5.0)).min(0.98).copysign(s)
    }
}

impl Agc {
    pub(crate) fn new() -> Self {
        Self { gain: 1.0, floor: 0.01 }
    }

    /// Усиливает кадр на месте; возвращает «пользователь говорит».
    /// `hold` — в динамиках звучит собеседник: усиление не
    /// поднимаем (иначе раздуем остаточное эхо) и «говорит» требует явно громкого сигнала.
    pub(crate) fn process(&mut self, frame: &mut [f32], hold: bool) -> bool {
        let rms = energy(frame).sqrt();
        if rms < self.floor {
            self.floor = self.floor * 0.8 + rms * 0.2;
        } else if !hold {
            self.floor += (rms - self.floor) * 0.003;
        }
        self.floor = self.floor.clamp(1e-4, 0.05);
        let speech = rms > (self.floor * 3.0).max(if hold { 0.06 } else { 0.004 });
        const TARGET: f32 = 0.125;
        let out_level = rms * self.gain;
        let next = if rms > 1e-5 && out_level > TARGET * 1.5 {
            (self.gain * 0.6 + (TARGET / rms) * 0.4).clamp(0.25, 20.0)
        } else if speech && !hold && out_level < TARGET * 0.7 {
            (self.gain * 1.06).min(20.0)
        } else {
            self.gain
        };
        let n = frame.len().max(1) as f32;
        for (i, s) in frame.iter_mut().enumerate() {
            let g = self.gain + (next - self.gain) * (i as f32 / n);
            *s = soft_clip(*s * g);
        }
        self.gain = next;
        speech && out_level.max(rms * next) > 0.02
    }
}

/// Без AGC: только мягкий лимитер, чтобы не хрипело.
pub(crate) fn limit(frame: &mut [f32]) -> bool {
    let rms = energy(frame).sqrt();
    frame.iter_mut().for_each(|s| *s = soft_clip(*s));
    rms > 0.02
}
