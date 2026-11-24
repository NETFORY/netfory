//! v1.135.230: голосовые звонки 1-на-1 (ALPN `netfory/voice/1`) — как tincan-cli:
//! Opus (48 кГц, моно, 20 мс, VoIP) в QUIC-датаграммах iroh, прямое соединение
//! (через relay при неудаче hole-punching). Сигналинг — JSON-строки по bi-stream
//! того же соединения: offer{room,auth} → accept|decline|busy … bye.
//! `auth` = sha256(room_key ‖ "voice" ‖ room_id): позвонить может только тот,
//! с кем уже есть E2EE-комната. Один активный звонок на приложение.
//! v1.135.235: эхоподавитель PBFDAF + AGC (`dsp.rs`), групповой mesh с адресами участников,
//! периодическое досоединение mesh, приглашение в голосовой канал (`voice-group-invite`).

use super::dsp::{Aec, Agc};
use super::{cfg_get, load_room, now_ms, peer_node_id, room_key, store_inbound, Room, DB};
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncBufReadExt;

pub(crate) const VOICE_ALPN: &[u8] = b"netfory/voice/1";
const RATE: u32 = 48_000;
const FRAME: usize = 960; // 20 мс @ 48 кГц
/// Джиттер-буфер: стартуем воспроизведение с 2 кадров, срезаем хвост >200 мс.
const JITTER_MIN: usize = FRAME * 2;
const JITTER_MAX: usize = FRAME * 10;

type Ring = Arc<Mutex<VecDeque<f32>>>;

/// Одно медиа-соединение (1-на-1 — ровно одно; групповой канал — mesh до 6).
struct Link {
    peer: String,
    /// адрес участника (для имени в UI); «» — неизвестен (старый клиент)
    addr: String,
    conn: iroh::endpoint::Connection,
    ring: Ring,
    speaking: Arc<AtomicBool>,
}
type Links = Arc<Mutex<Vec<Link>>>;

struct Call {
    room_id: String,
    /// сигнальное соединение 1-на-1 (None — групповой канал без «хозяина»)
    conn: Option<iroh::endpoint::Connection>,
    ctl: Option<Arc<tokio::sync::Mutex<iroh::endpoint::SendStream>>>,
    stop: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    /// стоп рингтона / гудков (сигнал до ответа)
    ring: Arc<AtomicBool>,
    /// медиа-линки (v1.135.232: общий микшер для 1-на-1 и группового канала)
    links: Links,
    /// момент начала разговора (мс) — для строки «☎ Звонок · mm:ss» в истории
    started: Arc<std::sync::atomic::AtomicU64>,
    group: bool,
}

fn new_links() -> Links {
    Arc::new(Mutex::new(Vec::new()))
}
/// Добавить медиа-линк; повторное соединение с тем же пиром (гонка взаимного join) заменяет старое.
fn add_link(links: &Links, peer: String, addr: String, conn: iroh::endpoint::Connection, stop: Arc<AtomicBool>) {
    let link = Link { peer: peer.clone(), addr, conn: conn.clone(), ring: Arc::new(Mutex::new(VecDeque::with_capacity(JITTER_MAX))), speaking: Arc::new(AtomicBool::new(false)) };
    let (ring, speaking) = (link.ring.clone(), link.speaking.clone());
    {
        let mut g = links.lock().unwrap();
        if let Some(i) = g.iter().position(|l| l.peer == peer) {
            g.remove(i).conn.close(0u32.into(), b"dup");
        }
        g.push(link);
    }
    spawn_rx(conn, ring, speaking, stop, links.clone());
}
fn linked_peers(links: &Links) -> Vec<String> {
    links.lock().unwrap().iter().map(|l| l.peer.clone()).collect()
}
fn my_addr() -> String {
    super::my_address().unwrap_or_default()
}

// ---- настройки аудио (v1.135.231): устройства, RNNoise, уровень ---------------

fn cfg_str(key: &str) -> String {
    cfg_get(key).and_then(|v| String::from_utf8(v).ok()).unwrap_or_default()
}
fn cfg_put(key: &str, val: &str) {
    let _ = DB.insert(key.as_bytes(), val.as_bytes());
}
/// Шумоподавление RNNoise (nnnoiseless) — по умолчанию ВКЛ, как у tincan.
fn denoise_enabled() -> bool {
    !matches!(cfg_get("chat:cfg:denoise"), Some(v) if v == b"0")
}
/// Эхоподавитель (AEC) — по умолчанию ВКЛ (звонки без наушников).
fn aec_enabled() -> bool {
    !matches!(cfg_get("chat:cfg:aec"), Some(v) if v == b"0")
}
/// Автоусиление микрофона — по умолчанию ВКЛ.
fn agc_enabled() -> bool {
    !matches!(cfg_get("chat:cfg:agc"), Some(v) if v == b"0")
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AudioSettings {
    inputs: Vec<String>,
    outputs: Vec<String>,
    input: String,
    output: String,
    denoise: bool,
    aec: bool,
    agc: bool,
}

/// Список устройств + текущий выбор («» = системное по умолчанию).
#[tauri::command]
pub(crate) async fn voice_audio_settings() -> Result<AudioSettings, String> {
    tauri::async_runtime::spawn_blocking(|| {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        fn names<I: Iterator<Item = cpal::Device>>(it: Result<I, cpal::DevicesError>) -> Vec<String> {
            it.map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
        }
        AudioSettings {
            inputs: names(host.input_devices()),
            outputs: names(host.output_devices()),
            input: cfg_str("chat:cfg:mic"),
            output: cfg_str("chat:cfg:spk"),
            denoise: denoise_enabled(),
            aec: aec_enabled(),
            agc: agc_enabled(),
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub(crate) fn voice_set_audio(input: Option<String>, output: Option<String>, denoise: Option<bool>, aec: Option<bool>, agc: Option<bool>) {
    if let Some(i) = input {
        cfg_put("chat:cfg:mic", &i);
    }
    if let Some(o) = output {
        cfg_put("chat:cfg:spk", &o);
    }
    if let Some(d) = denoise {
        cfg_put("chat:cfg:denoise", if d { "1" } else { "0" });
    }
    if let Some(a) = aec {
        cfg_put("chat:cfg:aec", if a { "1" } else { "0" });
    }
    if let Some(a) = agc {
        cfg_put("chat:cfg:agc", if a { "1" } else { "0" });
    }
}

fn pick_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    use cpal::traits::{DeviceTrait, HostTrait};
    if !name.is_empty() {
        let found = if input { host.input_devices().ok() } else { host.output_devices().ok() }
            .and_then(|mut it| it.find(|d| d.name().map(|n| n == name).unwrap_or(false)));
        if found.is_some() {
            return found;
        }
    }
    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

static METER: AtomicBool = AtomicBool::new(false);

/// Индикатор уровня микрофона (событие `voice-level` {level 0..1} ~10 раз/с).
#[tauri::command]
pub(crate) fn voice_meter(on: bool) {
    if !on {
        METER.store(false, Ordering::SeqCst);
        return;
    }
    if METER.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("voice-meter".into())
        .spawn(|| {
            use cpal::traits::{DeviceTrait, StreamTrait};
            let host = cpal::default_host();
            let Some(dev) = pick_device(&host, &cfg_str("chat:cfg:mic"), true) else {
                METER.store(false, Ordering::SeqCst);
                return;
            };
            let Ok(cfg) = dev.default_input_config() else {
                METER.store(false, Ordering::SeqCst);
                return;
            };
            let acc = Arc::new(Mutex::new((0f64, 0usize)));
            let a2 = acc.clone();
            let err_fn = |_e| {};
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::I16 => dev.build_input_stream(
                    &cfg.config(),
                    move |d: &[i16], _: &_| {
                        let mut g = a2.lock().unwrap();
                        for s in d {
                            let f = *s as f64 / 32768.0;
                            g.0 += f * f;
                        }
                        g.1 += d.len();
                    },
                    err_fn,
                    None,
                ),
                _ => dev.build_input_stream(
                    &cfg.config(),
                    move |d: &[f32], _: &_| {
                        let mut g = a2.lock().unwrap();
                        for s in d {
                            g.0 += (*s as f64) * (*s as f64);
                        }
                        g.1 += d.len();
                    },
                    err_fn,
                    None,
                ),
            };
            let Ok(stream) = stream else {
                METER.store(false, Ordering::SeqCst);
                return;
            };
            let _ = stream.play();
            while METER.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
                let (sum, n) = {
                    let mut g = acc.lock().unwrap();
                    let v = *g;
                    *g = (0.0, 0);
                    v
                };
                let rms = if n > 0 { (sum / n as f64).sqrt() } else { 0.0 };
                // dBFS → 0..1 (−60 дБ … 0 дБ)
                let level = if rms > 0.0 { ((20.0 * rms.log10() + 60.0) / 60.0).clamp(0.0, 1.0) } else { 0.0 };
                if let Some(app) = super::gossip::app_handle() {
                    crate::syslog::emit_queued(&app, "voice-level", serde_json::json!({ "level": level }));
                }
            }
            drop(stream);
        })
        .ok();
}

/// Рингтон (входящий: 440+480 Гц, 1с/2с) или гудки (исходящий: 425 Гц, 1с/4с) до ответа.
fn ring_tone(stop: Arc<AtomicBool>, incoming: bool) {
    std::thread::Builder::new()
        .name("voice-ring".into())
        .spawn(move || {
            use cpal::traits::{DeviceTrait, StreamTrait};
            let host = cpal::default_host();
            let Some(dev) = pick_device(&host, &cfg_str("chat:cfg:spk"), false) else { return };
            let Ok(cfg) = dev.default_output_config() else { return };
            let (rate, ch) = (cfg.sample_rate().0 as f32, cfg.channels() as usize);
            let (on_s, period_s) = if incoming { (1.0f32, 3.0f32) } else { (1.0, 5.0) };
            let mut t = 0f32;
            let stream = dev.build_output_stream(
                &cfg.config(),
                move |out: &mut [f32], _: &_| {
                    for frame in out.chunks_mut(ch) {
                        let phase = t % period_s;
                        let v = if phase < on_s {
                            let env = (phase * 40.0).min(1.0) * ((on_s - phase) * 40.0).min(1.0);
                            let w = 2.0 * std::f32::consts::PI * t;
                            if incoming {
                                0.12 * env * ((w * 440.0).sin() + (w * 480.0).sin())
                            } else {
                                0.15 * env * (w * 425.0).sin()
                            }
                        } else {
                            0.0
                        };
                        for s in frame.iter_mut() {
                            *s = v;
                        }
                        t += 1.0 / rate;
                    }
                },
                |_e| {},
                None,
            );
            let Ok(stream) = stream else { return };
            let _ = stream.play();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            drop(stream);
        })
        .ok();
}

/// Пропущенный / неотвеченный звонок → строка в истории (бейдж непрочитанного в списке чатов).
fn log_missed(room_id: &str, text: &str) {
    if let (Some(app), Some(room)) = (super::gossip::app_handle(), load_room(room_id)) {
        let peer = room.peer_address.clone();
        store_inbound(&app, &room, &peer, text, None, None);
    }
}

static CALL: Lazy<Mutex<Option<Call>>> = Lazy::new(|| Mutex::new(None));
/// Входящий, ждущий ответа пользователя (room_id, соединение, управляющий поток).
static PENDING: Lazy<Mutex<Option<Call>>> = Lazy::new(|| Mutex::new(None));

fn auth_for(room: &Room) -> Option<String> {
    let key = room_key(room).ok()?;
    let mut h = Sha256::new();
    h.update(key);
    h.update(b"voice");
    h.update(room.id.as_bytes());
    Some(hex::encode(h.finalize()))
}

fn emit_state(room_id: &str, state: &str, reason: &str) {
    // Android Telecom (BT-гарнитуры, системный маршрут звука): calling / active / ended
    if matches!(state, "calling" | "active" | "ended") {
        let label = load_room(room_id)
            .map(|r| r.peer_name.map(|n| format!("@{n}")).unwrap_or(r.peer_address))
            .unwrap_or_default();
        crate::android::call_state(room_id, state, reason, &label);
    }
    if let Some(app) = super::gossip::app_handle() {
        crate::syslog::emit_queued(
            &app,
            "voice-state",
            serde_json::json!({ "roomId": room_id, "state": state, "reason": reason, "ts": now_ms() }),
        );
    }
}

fn busy() -> bool {
    CALL.lock().unwrap().is_some()
}

/// Завершить активный звонок (локально): стоп аудио, закрыть соединение.
fn teardown(reason: &str) {
    let call = CALL.lock().unwrap().take();
    if let Some(c) = call {
        c.stop.store(true, Ordering::SeqCst);
        c.ring.store(true, Ordering::SeqCst);
        crate::tray_ring(false);
        if let Some(conn) = &c.conn {
            conn.close(0u32.into(), b"bye");
        }
        for l in c.links.lock().unwrap().iter() {
            l.conn.close(0u32.into(), b"bye");
        }
        let started = c.started.load(Ordering::SeqCst);
        if started > 0 {
            let secs = now_ms().saturating_sub(started) / 1000;
            log_local(&c.room_id, &format!("☎ {} · {:02}:{:02}", if c.group { "Голосовой канал" } else { "Звонок" }, secs / 60, secs % 60));
        }
        emit_state(&c.room_id, "ended", reason);
    }
}

/// Локальная строка истории (своя, без бейджа непрочитанного) + обновление ленты.
fn log_local(room_id: &str, text: &str) {
    let (Some(app), Some(room), Some(me)) = (super::gossip::app_handle(), load_room(room_id), super::my_address()) else { return };
    if super::store_msg_with_id(&room, &me, text, true, None, None).is_ok() {
        crate::syslog::emit_queued(&app, "chat-incoming", serde_json::json!({ "roomId": room.id, "sender": me, "text": text, "ts": now_ms(), "notify": false }));
    }
}

// ---- сигналинг --------------------------------------------------------------

/// Исходящий звонок в личку `room_id`.
#[tauri::command]
pub(crate) async fn voice_call(room_id: String) -> Result<(), String> {
    if busy() || PENDING.lock().unwrap().is_some() {
        return Err("Уже идёт звонок".into());
    }
    let room = load_room(&room_id).ok_or("Комната не найдена")?;
    if room.kind != "direct" {
        return Err("Звонки доступны только в личных чатах".into());
    }
    let node = peer_node_id(&room.peer_address).ok_or("У собеседника не опубликован P2P-узел")?;
    let id = node.parse::<iroh::EndpointId>().map_err(|e| e.to_string())?;
    let auth = auth_for(&room).ok_or("Нет ключа комнаты")?;
    let endpoint = crate::p2p::chat_endpoint().await.ok_or("P2P ещё не поднят")?;
    emit_state(&room_id, "calling", "");
    let conn = match tokio::time::timeout(std::time::Duration::from_secs(20), endpoint.connect(id, VOICE_ALPN)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            emit_state(&room_id, "ended", "unreachable");
            return Err(format!("Собеседник недоступен: {e}"));
        }
        Err(_) => {
            emit_state(&room_id, "ended", "unreachable");
            return Err("Собеседник недоступен (таймаут)".into());
        }
    };
    let (mut send, recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let offer = serde_json::json!({ "t": "offer", "room": room_id, "auth": auth, "addr": my_addr() }).to_string() + "\n";
    send.write_all(offer.as_bytes()).await.map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let muted = Arc::new(AtomicBool::new(false));
    let ring = Arc::new(AtomicBool::new(false));
    ring_tone(ring.clone(), false);
    let ctl = Arc::new(tokio::sync::Mutex::new(send));
    let (links, started) = (new_links(), Arc::new(std::sync::atomic::AtomicU64::new(0)));
    *CALL.lock().unwrap() = Some(Call {
        room_id: room_id.clone(),
        conn: Some(conn.clone()),
        ctl: Some(ctl.clone()),
        stop: stop.clone(),
        muted: muted.clone(),
        ring: ring.clone(),
        links: links.clone(),
        started: started.clone(),
        group: false,
    });
    // ждём accept/decline (45с), затем читаем bye до конца звонка
    let rid = room_id.clone();
    let peer_addr = room.peer_address.clone();
    tauri::async_runtime::spawn(async move {
        let mut lines = tokio::io::BufReader::new(recv).lines();
        let first = tokio::time::timeout(std::time::Duration::from_secs(45), lines.next_line()).await;
        let t = match first {
            Ok(Ok(Some(l))) => serde_json::from_str::<serde_json::Value>(&l).ok().and_then(|j| j["t"].as_str().map(|s| s.to_string())),
            _ => None,
        };
        ring.store(true, Ordering::SeqCst);
        match t.as_deref() {
            Some("accept") => {
                emit_state(&rid, "active", "");
                started.store(now_ms(), Ordering::SeqCst);
                add_link(&links, conn.remote_id().to_string(), peer_addr, conn.clone(), stop.clone());
                start_media(links.clone(), stop.clone(), muted.clone(), rid.clone());
                control_loop(rid, lines, stop).await;
            }
            Some("busy") => teardown("busy"),
            Some("decline") => teardown("declined"),
            _ => {
                log_missed(&rid, "☎ Исходящий звонок без ответа");
                teardown("no-answer");
            }
        }
    });
    Ok(())
}

/// Чтение управляющего потока до bye / обрыва → завершение звонка.
async fn control_loop(
    rid: String,
    mut lines: tokio::io::Lines<tokio::io::BufReader<iroh::endpoint::RecvStream>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await {
            Ok(Ok(Some(l))) => {
                if l.contains("\"bye\"") {
                    break;
                }
            }
            Ok(_) => break,
            Err(_) => continue,
        }
    }
    if CALL.lock().unwrap().as_ref().map(|c| c.room_id == rid).unwrap_or(false) {
        teardown("remote-hangup");
    }
    let cancelled = PENDING.lock().unwrap().take_if(|p| p.room_id == rid);
    if let Some(p) = cancelled {
        p.ring.store(true, Ordering::SeqCst);
        crate::tray_ring(false);
        crate::android::call_ended();
        log_missed(&rid, "☎ Пропущенный звонок");
        emit_state(&rid, "ended", "missed");
    }
}

/// Ответ на входящий: принять / отклонить.
#[tauri::command]
pub(crate) async fn voice_answer(accept: bool) -> Result<(), String> {
    let pending = PENDING.lock().unwrap().take().ok_or("Нет входящего звонка")?;
    pending.ring.store(true, Ordering::SeqCst);
    crate::tray_ring(false);
    crate::android::call_ended();
    let reply = if accept { "accept" } else { "decline" };
    if let Some(ctl) = &pending.ctl {
        let mut s = ctl.lock().await;
        let _ = s.write_all(format!("{{\"t\":\"{reply}\"}}\n").as_bytes()).await;
    }
    let Some(pconn) = pending.conn.clone() else { return Err("Нет соединения".into()) };
    if !accept {
        pconn.close(0u32.into(), b"decline");
        emit_state(&pending.room_id, "ended", "declined-local");
        return Ok(());
    }
    emit_state(&pending.room_id, "active", "");
    pending.started.store(now_ms(), Ordering::SeqCst);
    let peer_addr = load_room(&pending.room_id).map(|r| r.peer_address).unwrap_or_default();
    add_link(&pending.links, pconn.remote_id().to_string(), peer_addr, pconn, pending.stop.clone());
    start_media(pending.links.clone(), pending.stop.clone(), pending.muted.clone(), pending.room_id.clone());
    *CALL.lock().unwrap() = Some(pending);
    Ok(())
}

/// Android: маршрут звука активного звонка (speaker | earpiece | bluetooth | wired) через Telecom.
#[tauri::command]
pub(crate) fn voice_audio_route(route: String) -> Result<(), String> {
    crate::android::audio_route(&route)
}

#[tauri::command]
pub(crate) async fn voice_hangup() -> Result<(), String> {
    if let Some(p) = PENDING.lock().unwrap().take() {
        p.ring.store(true, Ordering::SeqCst);
        crate::tray_ring(false);
        crate::android::call_ended();
        if let Some(conn) = &p.conn {
            conn.close(0u32.into(), b"decline");
        }
        emit_state(&p.room_id, "ended", "declined-local");
    }
    let ctl = CALL.lock().unwrap().as_ref().and_then(|c| c.ctl.clone());
    if let Some(ctl) = ctl {
        let mut s = ctl.lock().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), s.write_all(b"{\"t\":\"bye\"}\n")).await;
    }
    teardown("local-hangup");
    Ok(())
}

#[tauri::command]
pub(crate) fn voice_mute(on: bool) -> bool {
    if let Some(c) = CALL.lock().unwrap().as_ref() {
        c.muted.store(on, Ordering::SeqCst);
        return on;
    }
    false
}

/// Приёмник входящих звонков.
#[derive(Debug, Clone)]
pub(crate) struct VoiceHandler;

impl iroh::protocol::ProtocolHandler for VoiceHandler {
    async fn accept(&self, connection: iroh::endpoint::Connection) -> Result<(), iroh::protocol::AcceptError> {
        let (mut send, recv) = connection.accept_bi().await?;
        let mut lines = tokio::io::BufReader::new(recv).lines();
        let Ok(Ok(Some(first))) = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line()).await else {
            return Ok(());
        };
        let Ok(j) = serde_json::from_str::<serde_json::Value>(&first) else { return Ok(()) };
        let room_id = j["room"].as_str().unwrap_or("").to_string();
        let room = match load_room(&room_id) {
            Some(r) if auth_for(&r).as_deref() == j["auth"].as_str() => r,
            _ => {
                crate::syslog::log("chat", "voice: offer с неверным auth отклонён");
                return Ok(());
            }
        };
        if j["t"].as_str() == Some("join") {
            let from = j["addr"].as_str().unwrap_or("").to_string();
            return group_accept(room_id, from, connection, send, lines).await;
        }
        if busy() || PENDING.lock().unwrap().is_some() {
            let _ = send.write_all(b"{\"t\":\"busy\"}\n").await;
            connection.close(0u32.into(), b"busy");
            return Ok(());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let ring = Arc::new(AtomicBool::new(false));
        let call = Call {
            room_id: room_id.clone(),
            conn: Some(connection.clone()),
            ctl: Some(Arc::new(tokio::sync::Mutex::new(send))),
            stop: stop.clone(),
            muted: Arc::new(AtomicBool::new(false)),
            ring: ring.clone(),
            links: new_links(),
            started: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            group: false,
        };
        *PENDING.lock().unwrap() = Some(call);
        ring_tone(ring.clone(), true);
        crate::tray_ring(true);
        if let Some(app) = super::gossip::app_handle() {
            crate::syslog::emit_queued(
                &app,
                "voice-incoming",
                serde_json::json!({ "roomId": room_id, "peerAddress": room.peer_address, "peerName": room.peer_name }),
            );
        }
        emit_state(&room_id, "ringing", "");
        crate::android::incoming_call(room.peer_name.as_deref().map(|n| format!("@{n}")).unwrap_or_else(|| room.peer_address.clone()).as_str(), &room_id);
        // 45с на ответ, иначе — пропущенный
        let rid = room_id.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(45)).await;
            let timed_out = PENDING.lock().unwrap().as_ref().map(|p| p.room_id == rid).unwrap_or(false);
            if timed_out {
                if let Some(p) = PENDING.lock().unwrap().take() {
                    p.ring.store(true, Ordering::SeqCst);
                    if let Some(conn) = &p.conn {
                        conn.close(0u32.into(), b"timeout");
                    }
                }
                crate::tray_ring(false);
                crate::android::call_ended();
                log_missed(&rid, "☎ Пропущенный звонок");
                emit_state(&rid, "ended", "missed");
            }
        });
        control_loop(room_id, lines, stop).await;
        Ok(())
    }
}

// ---- групповой голосовой канал (v1.135.232): mesh до 6 участников ---------------
//  join{room,auth} по VOICE_ALPN каждому известному участнику; те, кто в канале,
//  отвечают in{members} и держат соединение как медиа-линк; остальные — out.
//  Все линки получают одни и те же Opus-датаграммы; приём микшируется.

const GROUP_MAX: usize = 6;
/// Период досоединения mesh: участники, вошедшие в канал позже или недоступные при входе.
const GROUP_REMESH_SECS: u64 = 15;

/// Контекст группового канала для дозвона участникам.
#[derive(Clone)]
struct GroupCtx {
    room_id: String,
    auth: String,
    endpoint: iroh::Endpoint,
    links: Links,
    stop: Arc<AtomicBool>,
}

/// Разбор `members` ответа in: новый формат [{node,addr}] и старый [node].
fn parse_members(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|ms| ms.iter().filter_map(|m| m.as_str().or_else(|| m["node"].as_str()).map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// Дозвон join{room,auth,addr} по списку узлов (с расширением по members из ответов).
/// Возвращает число новых линков.
async fn group_dial(ctx: &GroupCtx, candidates: Vec<String>, max_tries: usize) -> usize {
    let me = crate::p2p::chat_endpoint_id();
    let mut tried: std::collections::BTreeSet<String> = linked_peers(&ctx.links).into_iter().collect();
    let mut queue: VecDeque<String> = candidates.into_iter().collect();
    let mut added = 0usize;
    let mut tries = 0usize;
    while let Some(node) = queue.pop_front() {
        if tries >= max_tries || ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        if Some(&node) == me.as_ref() || !tried.insert(node.clone()) {
            continue;
        }
        if ctx.links.lock().unwrap().len() >= GROUP_MAX - 1 {
            break;
        }
        tries += 1;
        let Ok(id) = node.parse::<iroh::EndpointId>() else { continue };
        let Ok(Ok(conn)) = tokio::time::timeout(std::time::Duration::from_secs(6), ctx.endpoint.connect(id, VOICE_ALPN)).await else { continue };
        let Ok((mut send, recv)) = conn.open_bi().await else { continue };
        let req = serde_json::json!({ "t": "join", "room": ctx.room_id, "auth": ctx.auth, "addr": my_addr() }).to_string() + "\n";
        if send.write_all(req.as_bytes()).await.is_err() {
            continue;
        }
        let mut lines = tokio::io::BufReader::new(recv).lines();
        let Ok(Ok(Some(l))) = tokio::time::timeout(std::time::Duration::from_secs(8), lines.next_line()).await else {
            conn.close(0u32.into(), b"timeout");
            continue;
        };
        let Ok(j) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
        if j["t"].as_str() == Some("in") {
            queue.extend(parse_members(&j["members"]));
            let addr = j["addr"].as_str().unwrap_or("").to_string();
            add_link(&ctx.links, node.clone(), addr, conn.clone(), ctx.stop.clone());
            added += 1;
            // контроль: держим send открытым, читаем до bye/обрыва → удаление линка
            let (links2, stop2) = (ctx.links.clone(), ctx.stop.clone());
            tauri::async_runtime::spawn(async move {
                let _keep = send;
                while !stop2.load(Ordering::SeqCst) {
                    match tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await {
                        Ok(Ok(Some(l))) if !l.contains("\"bye\"") => continue,
                        Err(_) => continue,
                        _ => break,
                    }
                }
                drop_link(&links2, &node);
            });
        } else {
            conn.close(0u32.into(), b"out");
        }
    }
    added
}

/// Войти в голосовой канал группы `room_id`.
#[tauri::command]
pub(crate) async fn voice_group_join(room_id: String) -> Result<(), String> {
    if busy() || PENDING.lock().unwrap().is_some() {
        return Err("Уже идёт звонок".into());
    }
    let room = load_room(&room_id).ok_or("Комната не найдена")?;
    if room.kind != "group" {
        return Err("Голосовой канал — только для групп".into());
    }
    let auth = auth_for(&room).ok_or("Нет ключа комнаты")?;
    let endpoint = crate::p2p::chat_endpoint().await.ok_or("P2P ещё не поднят")?;
    let stop = Arc::new(AtomicBool::new(false));
    let muted = Arc::new(AtomicBool::new(false));
    let links = new_links();
    let started = Arc::new(std::sync::atomic::AtomicU64::new(now_ms()));
    let ctx = GroupCtx { room_id: room_id.clone(), auth, endpoint, links: links.clone(), stop: stop.clone() };
    // Регистрируем канал ДО дозвона: встречные join от участников сразу получают in, а не out.
    *CALL.lock().unwrap() = Some(Call {
        room_id: room_id.clone(),
        conn: None,
        ctl: None,
        stop: stop.clone(),
        muted: muted.clone(),
        ring: Arc::new(AtomicBool::new(true)),
        links: links.clone(),
        started,
        group: true,
    });
    emit_state(&room_id, "active", "");
    start_media(links.clone(), stop.clone(), muted, room_id.clone());
    let n = group_dial(&ctx, super::sync::shuffled(super::sync::known_member_nodes(&room)), 48).await;
    crate::syslog::log("chat", format!("voice: вошли в голосовой канал, участников в mesh: {n}"));
    // периодическое досоединение: кто недоступен при входе или пришёл позже без нас в списке
    tauri::async_runtime::spawn(async move {
        while !ctx.stop.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_secs(GROUP_REMESH_SECS)).await;
            if ctx.stop.load(Ordering::SeqCst) || ctx.links.lock().unwrap().len() >= GROUP_MAX - 1 {
                continue;
            }
            let Some(room) = load_room(&ctx.room_id) else { break };
            let linked = linked_peers(&ctx.links);
            let cands: Vec<String> = super::sync::shuffled(super::sync::known_member_nodes(&room)).into_iter().filter(|n| !linked.contains(n)).collect();
            if cands.is_empty() {
                continue;
            }
            let added = group_dial(&ctx, cands, 8).await;
            if added > 0 {
                crate::syslog::log("chat", format!("voice: mesh досоединён, +{added}"));
            }
        }
    });
    Ok(())
}

fn drop_link(links: &Links, peer: &str) {
    let mut g = links.lock().unwrap();
    if let Some(i) = g.iter().position(|l| l.peer == peer) {
        g.remove(i).conn.close(0u32.into(), b"bye");
    }
}

/// Входящий join в групповой канал: мы в этом канале → in{members} + линк; иначе out
/// (+ событие `voice-group-invite` в UI: в группе начался голосовой канал).
async fn group_accept(
    room_id: String,
    from_addr: String,
    connection: iroh::endpoint::Connection,
    mut send: iroh::endpoint::SendStream,
    mut lines: tokio::io::Lines<tokio::io::BufReader<iroh::endpoint::RecvStream>>,
) -> Result<(), iroh::protocol::AcceptError> {
    let peer = connection.remote_id().to_string();
    let in_channel: Option<(Links, Arc<AtomicBool>)> = CALL
        .lock()
        .unwrap()
        .as_ref()
        .filter(|c| c.group && c.room_id == room_id)
        .map(|c| (c.links.clone(), c.stop.clone()));
    let Some((links, stop)) = in_channel else {
        let _ = send.write_all(b"{\"t\":\"out\"}\n").await;
        connection.close(0u32.into(), b"out");
        if !busy() {
            if let Some(app) = super::gossip::app_handle() {
                crate::syslog::emit_queued(&app, "voice-group-invite", serde_json::json!({ "roomId": room_id, "from": from_addr, "ts": now_ms() }));
            }
        }
        return Ok(());
    };
    let members: Vec<serde_json::Value> = links.lock().unwrap().iter().map(|l| serde_json::json!({ "node": l.peer, "addr": l.addr })).collect();
    if members.len() >= GROUP_MAX - 1 {
        let _ = send.write_all(b"{\"t\":\"full\"}\n").await;
        connection.close(0u32.into(), b"full");
        return Ok(());
    }
    let reply = serde_json::json!({ "t": "in", "members": members, "addr": my_addr() }).to_string() + "\n";
    let _ = send.write_all(reply.as_bytes()).await;
    add_link(&links, peer.clone(), from_addr, connection, stop.clone());
    while !stop.load(Ordering::SeqCst) {
        match tokio::time::timeout(std::time::Duration::from_millis(500), lines.next_line()).await {
            Ok(Ok(Some(l))) if !l.contains("\"bye\"") => continue,
            Err(_) => continue,
            _ => break,
        }
    }
    drop_link(&links, &peer);
    Ok(())
}

// ---- медиа: cpal ↔ Opus ↔ QUIC-датаграммы (микшер по линкам) --------------------

/// Приём датаграмм одного линка: u16 seq + Opus → джиттер-кольцо линка + VAD «говорит».
fn spawn_rx(conn: iroh::endpoint::Connection, ring: Ring, speaking: Arc<AtomicBool>, stop: Arc<AtomicBool>, links: Links) {
    tauri::async_runtime::spawn(async move {
        let Ok(mut dec) = opus::Decoder::new(RATE, opus::Channels::Mono) else { return };
        let mut pcm = vec![0f32; FRAME];
        let mut last_seq: Option<u16> = None;
        let mut last_voice = std::time::Instant::now();
        while !stop.load(Ordering::SeqCst) {
            let Ok(Ok(d)) = tokio::time::timeout(std::time::Duration::from_millis(500), conn.read_datagram()).await else {
                if conn.close_reason().is_some() {
                    break;
                }
                if last_voice.elapsed() > std::time::Duration::from_millis(400) {
                    speaking.store(false, Ordering::SeqCst);
                }
                continue;
            };
            if d.len() < 3 {
                continue;
            }
            let seq = u16::from_be_bytes([d[0], d[1]]);
            if let Some(prev) = last_seq {
                if seq.wrapping_sub(prev) == 2 {
                    if let Ok(n) = dec.decode_float(&[], &mut pcm, false) {
                        ring.lock().unwrap().extend(pcm[..n].iter().copied());
                    }
                }
            }
            last_seq = Some(seq);
            if let Ok(n) = dec.decode_float(&d[2..], &mut pcm, false) {
                let rms = (pcm[..n].iter().map(|s| s * s).sum::<f32>() / n.max(1) as f32).sqrt();
                if rms > 0.01 {
                    speaking.store(true, Ordering::SeqCst);
                    last_voice = std::time::Instant::now();
                } else if last_voice.elapsed() > std::time::Duration::from_millis(400) {
                    speaking.store(false, Ordering::SeqCst);
                }
                let mut q = ring.lock().unwrap();
                q.extend(pcm[..n].iter().copied());
                if q.len() > JITTER_MAX {
                    let cut = q.len() - JITTER_MAX;
                    q.drain(..cut);
                }
            }
        }
        let peer = conn.remote_id().to_string();
        drop_link(&links, &peer);
        // 1-на-1: единственный линк оборвался → звонок окончен
        let solo = CALL
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| !c.group && c.conn.as_ref().map(|x| x.stable_id() == conn.stable_id()).unwrap_or(false))
            .unwrap_or(false);
        if solo {
            teardown("connection-lost");
        }
    });
}

/// Аудио-тред + индикатор «кто говорит» (voice-speaking, 5 Гц).
fn start_media(links: Links, stop: Arc<AtomicBool>, muted: Arc<AtomicBool>, room_id: String) {
    let me_speaking = Arc::new(AtomicBool::new(false));
    let (links2, stop2, me2) = (links.clone(), stop.clone(), me_speaking.clone());
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        while !stop2.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let peers: Vec<serde_json::Value> = links2
                .lock()
                .unwrap()
                .iter()
                .map(|l| serde_json::json!({ "id": l.peer, "addr": l.addr, "speaking": l.speaking.load(Ordering::SeqCst) }))
                .collect();
            let payload = serde_json::json!({ "roomId": room_id, "peers": peers, "me": me2.load(Ordering::SeqCst) });
            let ser = payload.to_string();
            if ser != last {
                last = ser;
                if let Some(app) = super::gossip::app_handle() {
                    crate::syslog::emit_queued(&app, "voice-speaking", payload);
                }
            }
        }
    });
    std::thread::Builder::new()
        .name("voice-audio".into())
        .spawn(move || {
            if let Err(e) = audio_thread(links, stop.clone(), muted, me_speaking) {
                crate::syslog::log("chat", format!("voice: аудио недоступно: {e}"));
                stop.store(true, Ordering::SeqCst);
                teardown("audio-error");
            }
        })
        .ok();
}

/// Сколько дальнего конца (48 кГц моно, то, что ушло в вывод) держим для эхоподавителя.
const FAR_MAX: usize = FRAME * 25;

fn audio_thread(links: Links, stop: Arc<AtomicBool>, muted: Arc<AtomicBool>, me_speaking: Arc<AtomicBool>) -> Result<(), String> {
    use cpal::traits::{DeviceTrait, StreamTrait};
    let host = cpal::default_host();
    let input = pick_device(&host, &cfg_str("chat:cfg:mic"), true).ok_or("нет микрофона")?;
    let output = pick_device(&host, &cfg_str("chat:cfg:spk"), false).ok_or("нет устройства вывода")?;
    let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
    let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
    let (in_rate, in_ch) = (in_cfg.sample_rate().0, in_cfg.channels() as usize);
    let (out_rate, out_ch) = (out_cfg.sample_rate().0, out_cfg.channels() as usize);
    let err_fn = |e| crate::syslog::log("chat", format!("voice: cpal: {e}"));
    let cap: Ring = Arc::new(Mutex::new(VecDeque::with_capacity(RATE as usize)));
    // опорный сигнал дальнего конца для AEC: ровно те 48 кГц-отсчёты, что ушли в вывод
    let far: Ring = Arc::new(Mutex::new(VecDeque::with_capacity(FAR_MAX)));

    // захват: любой формат → моно f32 48 кГц (линейный ресемплинг) → кольцо
    let cap_in = cap.clone();
    let mut pos = 0f64;
    let step = in_rate as f64 / RATE as f64;
    let mut push_mono = move |mono: &[f32]| {
        let mut q = cap_in.lock().unwrap();
        while (pos as usize) + 1 < mono.len() {
            let i = pos as usize;
            let frac = (pos - i as f64) as f32;
            q.push_back(mono[i] * (1.0 - frac) + mono[i + 1] * frac);
            pos += step;
        }
        pos -= mono.len().saturating_sub(1) as f64;
        if pos < 0.0 {
            pos = 0.0;
        }
        if q.len() > RATE as usize {
            let cut = q.len() - RATE as usize;
            q.drain(..cut);
        }
    };
    let to_mono = move |data: &[f32]| -> Vec<f32> { data.chunks(in_ch).map(|c| c.iter().sum::<f32>() / in_ch as f32).collect() };
    let in_stream = match in_cfg.sample_format() {
        cpal::SampleFormat::I16 => input.build_input_stream(
            &in_cfg.config(),
            move |d: &[i16], _: &_| {
                let f: Vec<f32> = d.iter().map(|s| *s as f32 / 32768.0).collect();
                push_mono(&to_mono(&f));
            },
            err_fn,
            None,
        ),
        _ => input.build_input_stream(&in_cfg.config(), move |d: &[f32], _: &_| push_mono(&to_mono(d)), err_fn, None),
    }
    .map_err(|e| e.to_string())?;

    // воспроизведение: микс колец всех линков (48 кГц моно) → частота/каналы устройства
    let links_out = links.clone();
    let far_out = far.clone();
    let out_step = RATE as f64 / out_rate as f64;
    let mut carry: Vec<f32> = Vec::new();
    let mut opos = 0f64;
    let out_stream = output
        .build_output_stream(
            &out_cfg.config(),
            move |out: &mut [f32], _: &_| {
                let frames = out.len() / out_ch;
                let need = (frames as f64 * out_step) as usize + 2;
                {
                    let g = links_out.lock().unwrap();
                    let mut fq = far_out.lock().unwrap();
                    while carry.len() < need {
                        let mut mix = 0f32;
                        let mut any = false;
                        for l in g.iter() {
                            let mut q = l.ring.lock().unwrap();
                            // старт воспроизведения линка — после JITTER_MIN, иначе тишина
                            if q.len() >= FRAME / 2 {
                                if let Some(s) = q.pop_front() {
                                    mix += s;
                                    any = true;
                                }
                            }
                        }
                        let s = if any { super::dsp::soft_clip(mix) } else { 0.0 };
                        carry.push(s);
                        fq.push_back(s);
                    }
                    if fq.len() > FAR_MAX {
                        let cut = fq.len() - FAR_MAX;
                        fq.drain(..cut);
                    }
                }
                for f in 0..frames {
                    let i = opos as usize;
                    let s = if i + 1 < carry.len() {
                        let frac = (opos - i as f64) as f32;
                        carry[i] * (1.0 - frac) + carry[i + 1] * frac
                    } else {
                        0.0
                    };
                    for c in 0..out_ch {
                        out[f * out_ch + c] = s;
                    }
                    opos += out_step;
                }
                let used = (opos as usize).min(carry.len());
                carry.drain(..used);
                opos -= used as f64;
            },
            err_fn,
            None,
        )
        .map_err(|e| e.to_string())?;
    in_stream.play().map_err(|e| e.to_string())?;
    out_stream.play().map_err(|e| e.to_string())?;

    // кодер: каждые 20 мс кадр → [AEC] → [RNNoise] → [AGC] → Opus → датаграммы всем линкам
    let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).map_err(|e| e.to_string())?;
    let _ = enc.set_bitrate(opus::Bitrate::Bits(32_000));
    let mut seq: u16 = 0;
    let mut frame = vec![0f32; FRAME];
    let mut far_frame = vec![0f32; FRAME];
    let mut out = vec![0u8; 1500];
    let mut denoiser = if denoise_enabled() { Some(nnnoiseless::DenoiseState::new()) } else { None };
    let mut dn_in = vec![0f32; nnnoiseless::DenoiseState::FRAME_SIZE];
    let mut dn_out = vec![0f32; nnnoiseless::DenoiseState::FRAME_SIZE];
    let mut aec = if aec_enabled() { Some(Aec::new()) } else { None };
    let mut agc = if agc_enabled() { Some(Agc::new()) } else { None };
    crate::syslog::log(
        "chat",
        format!("voice: аудио {}Гц/{}к → {}Гц/{}к, AEC={} RNNoise={} AGC={}", in_rate, in_ch, out_rate, out_ch, aec.is_some(), denoiser.is_some(), agc.is_some()),
    );
    let mut frames_done: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        let ready = {
            let mut q = cap.lock().unwrap();
            if q.len() >= FRAME {
                for s in frame.iter_mut() {
                    *s = q.pop_front().unwrap_or(0.0);
                }
                true
            } else {
                false
            }
        };
        if !ready {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }
        // опорный кадр дальнего конца — потребляем синхронно с кадром микрофона
        {
            let mut fq = far.lock().unwrap();
            for s in far_frame.iter_mut() {
                *s = fq.pop_front().unwrap_or(0.0);
            }
        }
        if let Some(a) = aec.as_mut() {
            a.process(&mut frame, &far_frame);
            frames_done += 1;
            if frames_done % 1500 == 0 {
                crate::syslog::log("chat", format!("voice: AEC ERLE ≈ {:.0} дБ", a.erle_db));
            }
        }
        if muted.load(Ordering::SeqCst) {
            me_speaking.store(false, Ordering::SeqCst);
            continue;
        }
        if let Some(dn) = denoiser.as_mut() {
            for chunk in frame.chunks_mut(nnnoiseless::DenoiseState::FRAME_SIZE) {
                for (d, s) in dn_in.iter_mut().zip(chunk.iter()) {
                    *d = s * 32767.0;
                }
                dn.process_frame(&mut dn_out, &dn_in);
                for (s, d) in chunk.iter_mut().zip(dn_out.iter()) {
                    *s = (d / 32767.0).clamp(-1.0, 1.0);
                }
            }
        }
        let hold = aec.as_ref().map(|a| a.far_active()).unwrap_or(false);
        let speaking = match agc.as_mut() {
            Some(a) => a.process(&mut frame, hold),
            None => super::dsp::limit(&mut frame),
        };
        me_speaking.store(speaking, Ordering::SeqCst);
        if let Ok(n) = enc.encode_float(&frame, &mut out) {
            let mut pkt = Vec::with_capacity(n + 2);
            pkt.extend_from_slice(&seq.to_be_bytes());
            pkt.extend_from_slice(&out[..n]);
            seq = seq.wrapping_add(1);
            let g = links.lock().unwrap();
            for l in g.iter() {
                let _ = l.conn.send_datagram(pkt.clone().into());
            }
        }
    }
    drop(in_stream);
    drop(out_stream);
    Ok(())
}
