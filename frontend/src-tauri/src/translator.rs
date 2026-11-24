// ============================================================
//  SmartNet // translator.rs — ЛОКАЛЬНЫЙ нейросетевой переводчик Стены.
//
//  Полностью приватный, изолированный слой перевода постов/комментариев.
//  Внешних API нет (никакого OpenAI/DeepL) — инференс строго локальный на CPU
//  через candle + квантованную модель Qwen2.5-1.5B-Instruct (GGUF Q8_0, ~1.89 ГБ).
//
//  Ключевые правила:
//    * Весь инференс и загрузка файлов — в tokio::task::spawn_blocking,
//      чтобы не блокировать Main Thread Tauri и не морозить P2P-сидирование.
//    * Лимит потоков CPU: max(1, num_cpus/2), но НЕ более 4 (RAYON_NUM_THREADS).
//    * Lazy-init: веса и токенизатор грузятся один раз и живут в памяти.
//    * Кэш переводов в Sled: `social:translation:<post_id>:<target_lang>`.
//    * Никаких unwrap() — все ошибки возвращаются как Result::Err(String).
//
//  Author: TechnoL0g
//  Created: 2026-06-26
// ============================================================

use crate::translator_config::{self, Architecture, ChatTemplate, EngineKind};
use crate::{ensure_data_dir, DB};
use candle_core::quantized::gguf_file;
use candle_core::{Device, Tensor};
use candle_transformers::generation::LogitsProcessor;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokenizers::Tokenizer;

// Константы модели больше НЕ хардкодятся: все параметры (URL, iroh CID,
// размер, архитектура, EOS-токены, chat-template) читаются из окружения
// через `translator_config`. См. документацию модуля.

const MAX_NEW_TOKENS: usize = 640;
// На GPU (CUDA/Metal) снимаем осторожный CPU-лимит длины: длинные посты
// переводятся целиком быстро. На CPU держим 640, чтобы не морозить UI/P2P.
const MAX_NEW_TOKENS_GPU: usize = 2048;
const LOW_RAM_BYTES: u64 = 1024 * 1024 * 1024; // 1 ГБ

/// Backends для загруженных весов: одна архитектура — одна ветка.
/// Отдельные типы (не trait object) — потому что candle-структуры не dyn-safe.
enum ModelBackend {
    Qwen2(candle_transformers::models::quantized_qwen2::ModelWeights),
    Qwen3(candle_transformers::models::quantized_qwen3::ModelWeights),
}

impl ModelBackend {
    fn forward(&mut self, input: &Tensor, pos: usize) -> candle_core::Result<Tensor> {
        match self {
            Self::Qwen2(m) => m.forward(input, pos),
            Self::Qwen3(m) => m.forward(input, pos),
        }
    }
}

/// Загруженный «движок» модели (инициализируется лениво, живёт в памяти).
struct Engine {
    model: ModelBackend,
    tokenizer: Tokenizer,
    device: Device,
}

static ENGINE: Lazy<Mutex<Option<Engine>>> = Lazy::new(|| Mutex::new(None));

/// Загружена ли LLM-модель перевода в память (~2 ГБ) — для переписи crashdiag.
pub(crate) fn engine_loaded() -> bool {
    ENGINE.lock().map(|g| g.is_some()).unwrap_or(false)
}
static DOWNLOADING: AtomicBool = AtomicBool::new(false);
static PREWARMING: AtomicBool = AtomicBool::new(false);
// Активный ускоритель инференса: "cpu" | "cuda" | "metal" (определяется при
// первой инициализации движка). Показывается на Стене как индикатор скорости.
static ACCEL: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new("cpu".to_string()));

fn models_dir(app: &AppHandle) -> PathBuf {
    ensure_data_dir(app).join("models")
}
fn model_path(app: &AppHandle) -> PathBuf {
    models_dir(app).join(&translator_config::config().model_file)
}
fn tokenizer_path(app: &AppHandle) -> PathBuf {
    models_dir(app).join(&translator_config::config().tokenizer_file)
}

/// Установлен ли языковой пакет. candle требует и модель, и tokenizer.json;
/// llama.cpp (Bonsai) — только GGUF (словарь встроен в файл модели).
fn is_installed(app: &AppHandle) -> bool {
    let ok = match translator_config::config().engine {
        EngineKind::LlamaCpp => model_path(app).exists(),
        EngineKind::Candle => model_path(app).exists() && tokenizer_path(app).exists(),
    };
    ok && gguf_sane(&model_path(app)).is_ok()
}

/// Проверка GGUF ДО передачи в candle/llama.cpp. Битый файл (обрыв записи при
/// падении/жёсткой перезагрузке, NTFS os error 1392) даёт мусорный заголовок:
/// читалка верит `tensor_count`/`kv_count` ~10^12 и в цикле раздувает Vec
/// удвоением до сотен ГБ — ровно картина утечки RSS 100+ ГБ. Поэтому: магия,
/// версия, разумные счётчики, размер файла как в конфиге (±1%).
fn gguf_sane(path: &std::path::Path) -> Result<(), String> {
    use std::io::Read;
    let cfg = translator_config::config();
    let meta = std::fs::metadata(path).map_err(|e| {
        crate::crashdiag::note_io_error(&e, "translator model (metadata)");
        format!("metadata: {e}")
    })?;
    if cfg.model_size > 0 {
        let lo = cfg.model_size - cfg.model_size / 100;
        let hi = cfg.model_size + cfg.model_size / 100;
        if meta.len() < lo || meta.len() > hi {
            return Err(format!("размер {} ≠ ожидаемого {}", meta.len(), cfg.model_size));
        }
    }
    let mut f = std::fs::File::open(path).map_err(|e| {
        crate::crashdiag::note_io_error(&e, "translator model (open)");
        format!("open: {e}")
    })?;
    let mut hdr = [0u8; 24];
    f.read_exact(&mut hdr).map_err(|e| {
        crate::crashdiag::note_io_error(&e, "translator model (read)");
        format!("header: {e}")
    })?;
    if &hdr[0..4] != b"GGUF" {
        return Err("нет магии GGUF".into());
    }
    let version = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let tensors = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
    let kvs = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
    if !(1..=3).contains(&version) || tensors == 0 || tensors > 100_000 || kvs > 10_000 {
        return Err(format!("мусорный заголовок: v{version}, tensors={tensors}, kv={kvs}"));
    }
    Ok(())
}

/// Битую модель убираем с дороги (переименование переживает даже NTFS-1392,
/// когда удаление невозможно) и логируем — bootstrap скачает заново.
fn quarantine_corrupt_model(app: &AppHandle, why: &str) {
    let p = model_path(app);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let q = p.with_extension(format!("corrupt-{ts}"));
    let moved = std::fs::rename(&p, &q).is_ok() || std::fs::remove_file(&p).is_ok();
    crate::syslog::log(
        "sys",
        format!(
            "AI translator: model file is CORRUPT ({why}) — {} · run `chkdsk /f` on this drive if this repeats",
            if moved { "quarantined, will re-download" } else { "could not move/delete it (NTFS damage?)" }
        ),
    );
    crate::crash_log(&format!("TRANSLATOR: corrupt model {} — {why}", p.display()));
}

// --- Лимит потоков CPU (вызывается один раз перед инференсом) ---
fn cpu_threads() -> usize {
    std::cmp::min(4, std::cmp::max(1, num_cpus::get() / 2))
}
fn apply_thread_limit() {
    let t = cpu_threads().to_string();
    // gemm/rayon читают эту переменную при инициализации пула.
    std::env::set_var("RAYON_NUM_THREADS", &t);
    std::env::set_var("CANDLE_NUM_THREADS", &t);
}

// --- Выбор устройства инференса (GPU-ускорение) ---
// По умолчанию — CPU (собирается на любой машине без доп. зависимостей).
// GPU включается ОПЦИОНАЛЬНО через cargo-фичи при локальной сборке:
//   * `--features cuda`  — внешняя NVIDIA-видеокарта (нужен установленный CUDA Toolkit);
//   * `--features metal` — Apple Silicon / macOS (Metal, из коробки).
// Если GPU выбран, но недоступен в рантайме — безопасный откат на CPU.
fn set_accel(name: &str) {
    if let Ok(mut g) = ACCEL.lock() {
        *g = name.to_string();
    }
}
fn select_device() -> Device {
    #[cfg(feature = "cuda")]
    {
        match Device::new_cuda(0) {
            Ok(d) => {
                set_accel("cuda");
                return d;
            }
            Err(e) => eprintln!("[translator] CUDA недоступна ({e}) — откат на CPU"),
        }
    }
    #[cfg(feature = "metal")]
    {
        match Device::new_metal(0) {
            Ok(d) => {
                set_accel("metal");
                return d;
            }
            Err(e) => eprintln!("[translator] Metal недоступен ({e}) — откат на CPU"),
        }
    }
    set_accel("cpu");
    Device::Cpu
}

// ============================================================
//  Статус / системные проверки
// ============================================================

#[derive(Serialize, Clone)]
pub(crate) struct TranslatorStatus {
    pub installed: bool,
    pub downloading: bool,
    pub disk_free_mb: u64,
    pub disk_ok: bool,
    pub ram_low: bool,
    pub threads: usize,
    // Активный ускоритель инференса: "cpu" | "cuda" | "metal".
    pub accelerator: String,
    // Настроенная модель — имя пресета/файла + supported-флаг.
    pub model_name: String,
    pub model_file: String,
    pub model_description: String,
    pub model_supported: bool,
    /// Причина, если !supported (для показа в UI).
    pub model_unsupported_reason: String,
    /// Размер пакета (модель + токенизатор), МБ — для честного «~N ГБ» в UI.
    pub model_size_mb: u64,
}

/// Свободное место на диске, где лежит папка данных (best-effort).
fn disk_free_bytes(app: &AppHandle) -> Option<u64> {
    use sysinfo::Disks;
    let target = ensure_data_dir(app);
    let disks = Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None;
    for d in disks.list() {
        let mp = d.mount_point();
        if target.starts_with(mp) {
            let len = mp.as_os_str().len();
            if best.map(|(l, _)| len > l).unwrap_or(true) {
                best = Some((len, d.available_space()));
            }
        }
    }
    best.map(|(_, b)| b)
}

fn ram_is_low() -> bool {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.available_memory() < LOW_RAM_BYTES
}

#[tauri::command]
pub(crate) async fn translator_status(app: AppHandle) -> TranslatorStatus {
    let installed = is_installed(&app);
    let free = disk_free_bytes(&app);
    let cfg = translator_config::config();
    TranslatorStatus {
        installed,
        downloading: DOWNLOADING.load(Ordering::Relaxed),
        disk_free_mb: free.map(|b| b / 1024 / 1024).unwrap_or(0),
        disk_ok: free.map(|b| b >= cfg.required_disk_bytes).unwrap_or(true),
        ram_low: ram_is_low(),
        threads: cpu_threads(),
        accelerator: ACCEL.lock().map(|g| g.clone()).unwrap_or_else(|_| "cpu".into()),
        model_name: cfg.name.to_string(),
        model_file: cfg.model_file.to_string(),
        model_description: cfg.description.to_string(),
        model_supported: cfg.supported,
        model_unsupported_reason: cfg.unsupported_reason.to_string(),
        model_size_mb: (cfg.model_size + cfg.tokenizer_size) / 1_000_000,
    }
}

/// Список доступных встроенных пресетов (для UI Settings). Возвращает
/// имя, описание и флаг supported, чтобы фронт мог показать выпадающий
/// список моделей и предупредить о неподдерживаемых форматах.
#[tauri::command]
pub(crate) fn translator_presets() -> Vec<TranslatorPreset> {
    translator_config::preset_names()
        .iter()
        .filter_map(|&n| {
            translator_config::preset_view(n).map(|(name, desc, sup, reason)| TranslatorPreset {
                name: name.to_string(),
                description: desc.to_string(),
                supported: sup,
                unsupported_reason: reason.to_string(),
            })
        })
        .collect()
}

#[derive(Serialize, Clone)]
pub(crate) struct TranslatorPreset {
    pub name: String,
    pub description: String,
    pub supported: bool,
    pub unsupported_reason: String,
}

// ============================================================
//  Фоновый загрузчик (ureq в spawn_blocking + прогресс через emit)
// ============================================================

#[derive(Serialize, Clone)]
pub(crate) struct DownloadProgress {
    phase: String, // "tokenizer" | "model" | "torrent" | "done" | "error"
    pct: f64,
    downloaded_mb: u64,
    total_mb: u64,
    error: Option<String>,
}

// Последний известный прогресс загрузки — чтобы фронтенд мог запросить
// актуальный статус при монтировании (после навигации по страницам),
// а не ждать следующего live-события.
static LAST_PROGRESS: Lazy<Mutex<Option<DownloadProgress>>> = Lazy::new(|| Mutex::new(None));

fn emit_progress(app: &AppHandle, p: DownloadProgress) {
    if let Ok(mut g) = LAST_PROGRESS.lock() {
        *g = Some(p.clone());
    }
    let _ = app.emit("translator-progress", p);
}

/// Текущий статус фоновой загрузки языковой модели (для восстановления
/// индикатора при возврате на страницу). None — если загрузка не начиналась.
#[tauri::command]
pub(crate) async fn translator_progress() -> Option<DownloadProgress> {
    LAST_PROGRESS.lock().ok().and_then(|g| g.clone())
}

/// Скачать один файл по URL в dest, транслируя прогресс (для фазы `phase`).
fn download_file(
    app: &AppHandle,
    url: &str,
    dest: &PathBuf,
    phase: &str,
    connect_timeout: Option<std::time::Duration>,
) -> Result<(), String> {
    // Для cloud-режима ограничиваем ВРЕМЯ УСТАНОВКИ соединения (доступность
    // источника), но НЕ весь download — большой файл качается сколько нужно.
    let resp = match connect_timeout {
        Some(t) => ureq::builder()
            .timeout_connect(t)
            .build()
            .get(url)
            .call()
            .map_err(|e| format!("сеть недоступна: {e}"))?,
        None => ureq::get(url)
            .call()
            .map_err(|e| format!("сеть недоступна: {e}"))?,
    };
    let total: u64 = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| {
        crate::crashdiag::note_io_error(&e, "translator download (create)");
        format!("файл: {e}")
    })?;
    use std::io::{Read, Write};
    let mut buf = vec![0u8; 1024 * 256];
    let mut done: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("чтение: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| {
            crate::crashdiag::note_io_error(&e, "translator download (write)");
            format!("запись: {e}")
        })?;
        done += n as u64;
        if last_emit.elapsed().as_millis() >= 300 {
            let pct = if total > 0 {
                (done as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            emit_progress(
                app,
                DownloadProgress {
                    phase: phase.to_string(),
                    pct,
                    downloaded_mb: done / 1024 / 1024,
                    total_mb: total / 1024 / 1024,
                    error: None,
                },
            );
            last_emit = std::time::Instant::now();
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, dest).map_err(|e| format!("переименование: {e}"))?;
    Ok(())
}

/// Запустить фоновую загрузку языкового пакета. Возвращается сразу.
///
/// Порядок источников: **HTTPS URL из конфига — основной**, iroh CID —
/// fallback (см. `run_bootstrap`). Публичная команда — legacy-оболочка,
/// выполняет ту же логику, что и `translator_bootstrap`.
#[tauri::command]
pub(crate) async fn translator_download(app: AppHandle) -> Result<(), String> {
    translator_bootstrap(app).await
}

// ============================================================
//  Bootstrap: скачивание языкового пакета через iroh blobs (единственный путь)
// ============================================================

/// Запустить фоновое скачивание языкового пакета через iroh (единственный путь).
/// Идемпотентно (флаг DOWNLOADING). Прогресс транслируется событием
/// `translator-progress`; по завершении файлы лежат в data/models.
#[tauri::command]
pub(crate) async fn translator_bootstrap(app: AppHandle) -> Result<(), String> {
    crate::syslog::log("sys", "AI translator: checking if language model is installed on disk…");
    if is_installed(&app) {
        crate::syslog::log("sys", "AI translator: model already installed — translation is ready");
        return Ok(());
    }
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        crate::syslog::log("sys", "AI translator: model download already in progress");
        return Ok(()); // уже идёт
    }
    crate::syslog::log(
        "sys",
        "AI translator: model not found — starting background download via iroh",
    );
    tauri::async_runtime::spawn(async move {
        let res = run_bootstrap(&app).await;
        DOWNLOADING.store(false, Ordering::SeqCst);
        match &res {
            Ok(()) => crate::syslog::log("sys", "AI translator: language pack installed ✓"),
            Err(e) => crate::syslog::log("sys", format!("AI translator: download error — {e}")),
        }
        if let Err(e) = res {
            emit_progress(
                &app,
                DownloadProgress {
                    phase: "error".to_string(),
                    pct: 0.0,
                    downloaded_mb: 0,
                    total_mb: 0,
                    error: Some(e),
                },
            );
        }
    });
    Ok(())
}

/// Скачать один blob через iroh в `dest`, транслируя прогресс (фоновый насос
/// читает blob_progress и шлёт translator-progress фазы `phase`). Файл
/// экспортируется iroh-хранилищем прямо в data/models (по пути `dest`).
async fn fetch_blob_with_progress(
    app: &AppHandle,
    iroh_node: &str,
    cid: &str,
    phase: &str,
    size: u64,
    dest: &PathBuf,
) -> Result<(), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let app_prog = app.clone();
    let cid_s = cid.to_string();
    let phase_s = phase.to_string();
    tauri::async_runtime::spawn(async move {
        loop {
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            let (dl, done) = crate::p2p::blob_progress(&app_prog, &cid_s).await;
            let pct = if size > 0 {
                ((dl as f64 / size as f64) * 100.0).min(100.0)
            } else {
                0.0
            };
            emit_progress(
                &app_prog,
                DownloadProgress {
                    phase: phase_s.clone(),
                    pct,
                    downloaded_mb: dl / 1024 / 1024,
                    total_mb: size / 1024 / 1024,
                    error: None,
                },
            );
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });
    let d = dest.to_string_lossy().to_string();
    // Ждём до ПОЛНОЙ загрузки: iroh download_complete_any делает лишь 5 попыток,
    // поэтому повторяем, пока файл не окажется на диске (без общего таймаута —
    // сеть может быть временно недоступна, а нам важно докачать пакет целиком).
    // СТОРОЖЕВОЙ ТАЙМЕР: после сна ноутбука QUIC-соединения iroh мертвы и
    // download() может висеть вечно, не возвращая управление, — поэтому каждую
    // попытку гоняем наперегонки с watchdog'ом: если байты blob не растут
    // STALL_SECS подряд, попытка отменяется (drop future) и стартует заново.
    const STALL_SECS: u64 = 120;
    loop {
        let fetch = crate::p2p::fetch_file(app, iroh_node, cid, &d, &[]);
        tokio::pin!(fetch);
        let stall = async {
            let mut last_bytes: u64 = 0;
            let mut last_change = std::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let (dl, done) = crate::p2p::blob_progress(app, cid).await;
                if done || dl > last_bytes {
                    last_bytes = dl.max(last_bytes);
                    last_change = std::time::Instant::now();
                }
                if last_change.elapsed().as_secs() >= STALL_SECS {
                    break;
                }
            }
        };
        let mut retry_secs = 60u64;
        let mut finished = false;
        tokio::select! {
            r = &mut fetch => match r {
                Ok(_) if dest.exists() => finished = true,
                Ok(_) => {
                    crate::syslog::log(
                        "sys",
                        format!("AI translator: {phase} — file did not download, retry in 60 s…"),
                    );
                }
                Err(e) => {
                    crate::syslog::log(
                        "sys",
                        format!("AI translator: {phase} — network unavailable ({e}), retry in 60 s…"),
                    );
                }
            },
            _ = stall => {
                // Зависшая попытка отброшена — переподключаемся почти сразу.
                crate::syslog::log(
                    "sys",
                    format!("AI translator: {phase} — no progress for {STALL_SECS} s (sleep/network drop?), reconnecting…"),
                );
                retry_secs = 5;
            }
        }
        if finished {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(retry_secs)).await;
    }
    stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// Скачать языковой пакет через iroh blobs (fallback, если HTTPS недоступен).
/// Требует, чтобы в текущем `ModelConfig` были заданы CID и node — иначе
/// возвращает ошибку "нет iroh fallback".
async fn iroh_fetch(app: &AppHandle) -> Result<(), String> {
    let cfg = translator_config::config();
    std::fs::create_dir_all(models_dir(app)).map_err(|e| e.to_string())?;
    if matches!(cfg.engine, EngineKind::Candle) && !tokenizer_path(app).exists() {
        if cfg.tokenizer_iroh_cid.is_empty() || cfg.tokenizer_iroh_node.is_empty() {
            return Err("iroh fallback: CID токенизатора не задан в конфиге".into());
        }
        fetch_blob_with_progress(
            app,
            &cfg.tokenizer_iroh_node,
            &cfg.tokenizer_iroh_cid,
            "tokenizer",
            cfg.tokenizer_size,
            &tokenizer_path(app),
        )
        .await?;
    }
    if !model_path(app).exists() {
        if cfg.model_iroh_cid.is_empty() || cfg.model_iroh_node.is_empty() {
            return Err("iroh fallback: CID модели не задан в конфиге".into());
        }
        fetch_blob_with_progress(
            app,
            &cfg.model_iroh_node,
            &cfg.model_iroh_cid,
            "model",
            cfg.model_size,
            &model_path(app),
        )
        .await?;
    }
    Ok(())
}

/// Скачать через HTTPS URL из конфига (основной путь, приоритет 1).
/// Не рекурсивно ретраит — при первой ошибке возвращаемся к вызывающей
/// стороне, которая может перейти на iroh fallback.
fn https_fetch(app: &AppHandle) -> Result<(), String> {
    let cfg = translator_config::config();
    std::fs::create_dir_all(models_dir(app)).map_err(|e| e.to_string())?;
    // Токенизатор нужен только candle-движку; llama.cpp берёт словарь из GGUF.
    if matches!(cfg.engine, EngineKind::Candle) && !tokenizer_path(app).exists() {
        if cfg.tokenizer_url.is_empty() {
            return Err("HTTPS: URL токенизатора не задан в конфиге".into());
        }
        download_file(app, &cfg.tokenizer_url, &tokenizer_path(app), "tokenizer", None)?;
    }
    if model_path(app).exists() {
        if let Err(why) = gguf_sane(&model_path(app)) {
            quarantine_corrupt_model(app, &why);
        }
    }
    if !model_path(app).exists() {
        if cfg.model_url.is_empty() {
            return Err("HTTPS: URL модели не задан в конфиге".into());
        }
        download_file(app, &cfg.model_url, &model_path(app), "model", None)?;
        gguf_sane(&model_path(app)).map_err(|why| {
            quarantine_corrupt_model(app, &why);
            format!("HTTPS: скачанный файл не прошёл проверку GGUF — {why}")
        })?;
    }
    Ok(())
}

/// Скачать модель через ПУБЛИЧНУЮ ссылку облачного диска Mail.ru (приоритет 3,
/// «cloud mode» — когда HTTPS и iroh недоступны). Ссылка берётся из env
/// `TRANSLATOR_MODEL_CLOUD` (https://cloud.mail.ru/public/<weblink>). Скачиваем
/// ТОЛЬКО модель (для Bonsai/llama.cpp этого достаточно — словарь в GGUF).
async fn cloud_fetch(app: &AppHandle) -> Result<(), String> {
    let link = std::env::var("TRANSLATOR_MODEL_CLOUD")
        .unwrap_or_default()
        .trim()
        .to_string();
    if link.is_empty() {
        return Err("cloud fallback: ссылка не задана (TRANSLATOR_MODEL_CLOUD)".into());
    }
    std::fs::create_dir_all(models_dir(app)).map_err(|e| e.to_string())?;
    if model_path(app).exists() {
        return Ok(());
    }
    // Анонимно резолвим публичную ссылку в прямой download-URL. Ограничиваем
    // «доступность» ~6с: если Mail.ru не ответил — быстро уходим на HTTPS/iroh.
    let direct = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        ntfry::seeder::mailru_public_direct_url(&link),
    )
    .await
    .map_err(|_| "Mail.ru не ответил за ~6с (тайм-аут доступности)".to_string())?
    .map_err(|e| format!("Mail.ru resolve: {e}"))?;
    let app2 = app.clone();
    let dest = model_path(app);
    // connect-timeout ~6с на само соединение; сам download не ограничен по времени.
    tauri::async_runtime::spawn_blocking(move || {
        download_file(
            &app2,
            &direct,
            &dest,
            "model",
            Some(std::time::Duration::from_secs(6)),
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    // Проверяем, что скачали именно GGUF (а не HTML-страницу ошибки Mail.ru).
    let dst = model_path(app);
    let mut magic = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open(&dst) {
        use std::io::Read;
        let _ = f.read_exact(&mut magic);
    }
    if &magic != b"GGUF" {
        let _ = std::fs::remove_file(&dst);
        return Err("cloud fallback: скачан не GGUF (возможно ссылка ведёт на папку/страницу, а не на файл)".into());
    }
    Ok(())
}

/// NTFS os error 1392 («файл или папка повреждены») на самой папке models —
/// ни создать, ни удалить файл нельзя, все три источника падают одинаково.
/// Лечим переименованием повреждённой папки и созданием новой; пользователю —
/// подсказка про chkdsk.
fn heal_models_dir(app: &AppHandle) {
    let dir = models_dir(app);
    let _ = std::fs::create_dir_all(&dir);
    let probe = dir.join(".probe");
    let err = match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            return;
        }
        Err(e) => e,
    };
    if !crate::crashdiag::note_io_error(&err, "translator models dir") {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let q = dir.with_file_name(format!("models.corrupt-{ts}"));
    let moved = std::fs::rename(&dir, &q).is_ok();
    let _ = std::fs::create_dir_all(&dir);
    let msg = format!(
        "AI translator: models folder is CORRUPT (NTFS os error 1392) — {} · run `chkdsk /f` on this drive to repair the disk",
        if moved { "renamed aside, fresh folder created, re-downloading" } else { "rename failed — disk repair required" }
    );
    crate::syslog::log("sys", msg.clone());
    crate::crash_log(&format!("TRANSLATOR: {msg}"));
}

async fn run_bootstrap(app: &AppHandle) -> Result<(), String> {
    heal_models_dir(app);
    if model_path(app).exists() {
        if let Err(why) = gguf_sane(&model_path(app)) {
            quarantine_corrupt_model(app, &why);
        }
    }
    let cfg = translator_config::config();
    // Отказать сразу, если выбрана неподдерживаемая архитектура (Bonsai Q1_0
    // и т.п.) — качать 570 МБ впустую бессмысленно.
    if !cfg.supported {
        return Err(format!(
            "Модель '{}' не поддерживается текущим движком. {}",
            cfg.name, cfg.unsupported_reason
        ));
    }
    // Проверка свободного места (жёстко).
    if let Some(free) = disk_free_bytes(app) {
        if free < cfg.required_disk_bytes {
            return Err(format!(
                "Недостаточно места на диске: нужно ~{} МБ, свободно {} МБ",
                cfg.required_disk_bytes / 1024 / 1024,
                free / 1024 / 1024
            ));
        }
    }
    // Приоритет источников: cloud (Mail.ru, TRANSLATOR_MODEL_CLOUD) → HTTPS → iroh.
    // cloud пробуем ПЕРВЫМ с коротким тайм-аутом доступности (~6с): если облако
    // не отвечает — быстро уходим на остальные методы.
    crate::syslog::log(
        "sys",
        format!(
            "AI translator: downloading {} (~{} MB), priority: cloud (Mail.ru) → HTTPS → iroh…",
            cfg.name,
            cfg.model_size / 1024 / 1024
        ),
    );
    let mut errs: Vec<String> = Vec::new();

    // 1) cloud (Mail.ru) — основной источник (если задан TRANSLATOR_MODEL_CLOUD).
    match cloud_fetch(app).await {
        Ok(()) => crate::syslog::log("sys", "AI translator: cloud (Mail.ru) — source ✓"),
        Err(e) => {
            crate::syslog::log(
                "sys",
                format!("AI translator: cloud unavailable ({e}), trying HTTPS…"),
            );
            errs.push(format!("cloud: {e}"));
        }
    }

    // 2) HTTPS — если ещё не всё на месте (для candle нужен и tokenizer).
    if !is_installed(app) {
        let app_https = app.clone();
        let https_res = tauri::async_runtime::spawn_blocking(move || https_fetch(&app_https))
            .await
            .map_err(|e| e.to_string())?;
        match https_res {
            Ok(()) => crate::syslog::log("sys", "AI translator: HTTPS — source ✓"),
            Err(e) => {
                crate::syslog::log(
                    "sys",
                    format!("AI translator: HTTPS failed ({e}), trying iroh…"),
                );
                errs.push(format!("HTTPS: {e}"));
            }
        }
    }

    // 3) iroh — последний fallback.
    if !is_installed(app) {
        match iroh_fetch(app).await {
            Ok(()) => crate::syslog::log("sys", "AI translator: iroh fallback succeeded ✓"),
            Err(e2) => errs.push(format!("iroh: {e2}")),
        }
    }

    if !is_installed(app) {
        return Err(format!("не удалось скачать модель: {}", errs.join(" | ")));
    }
    emit_progress(
        app,
        DownloadProgress {
            phase: "done".to_string(),
            pct: 100.0,
            downloaded_mb: 0,
            total_mb: 0,
            error: None,
        },
    );
    Ok(())
}

// ============================================================
//  Определение языка (whatlang) — для авторежима
// ============================================================

fn lang_to_2(l: whatlang::Lang) -> &'static str {
    use whatlang::Lang;
    match l {
        Lang::Rus => "ru",
        Lang::Eng => "en",
        Lang::Ind => "id",
        Lang::Ukr => "uk",
        Lang::Spa => "es",
        Lang::Por => "pt",
        Lang::Fra => "fr",
        Lang::Deu => "de",
        Lang::Ita => "it",
        Lang::Tur => "tr",
        Lang::Pol => "pl",
        Lang::Nld => "nl",
        Lang::Ara => "ar",
        Lang::Cmn => "zh",
        Lang::Jpn => "ja",
        Lang::Kor => "ko",
        Lang::Hin => "hi",
        _ => "",
    }
}

/// Определить язык текста (ISO-639-1, best-effort). Пустая строка — не определено.
#[tauri::command]
pub(crate) async fn detect_lang(text: String) -> String {
    tauri::async_runtime::spawn_blocking(move || {
        let t = text.trim();
        if t.chars().count() < 3 {
            return String::new();
        }
        whatlang::detect(t)
            .filter(|i| i.confidence() >= 0.5)
            .map(|i| lang_to_2(i.lang()).to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

// ============================================================
//  Перевод (Sled-кэш → инференс)
// ============================================================
//
//  Умный кэш переводов в Sled с защитой от устаревания и лимитом на автора:
//    * Ключ: `social:translation:<author>:<post_id>:<target_lang>`
//    * Значение (JSON): { src_text_hash, translated_text, timestamp }
//    * Инвалидация: при запросе сверяем blake3-хэш ТЕКУЩЕГО текста с сохранённым
//      src_text_hash. Не совпал (пост изменён/дополнен) → стираем запись и
//      переводим заново.
//    * Лимит памяти: не более MAX_CACHE_PER_AUTHOR (200) записей на автора —
//      при переполнении стираются самые старые по timestamp.

const MAX_CACHE_PER_AUTHOR: usize = 200;

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    src_text_hash: String,
    translated_text: String,
    timestamp: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// blake3-хэш оригинального текста (для инвалидации устаревших переводов).
fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn cache_key(author: &str, post_id: &str, target: &str) -> String {
    format!("social:translation:{}:{}:{}", author, post_id, target)
}

fn author_prefix(author: &str) -> String {
    format!("social:translation:{}:", author)
}

/// Прочитать перевод из кэша с проверкой хэша оригинала. Если запись устарела
/// (текст поста изменился) — стираем её и возвращаем None (форсируем инференс).
fn cache_lookup(author: &str, post_id: &str, target: &str, text: &str) -> Option<String> {
    let key = cache_key(author, post_id, target);
    let raw = DB.get(key.as_bytes()).ok().flatten()?;
    let entry: CacheEntry = serde_json::from_slice(&raw).ok()?;
    if entry.src_text_hash == content_hash(text) {
        Some(entry.translated_text)
    } else {
        let _ = DB.remove(key.as_bytes());
        let _ = DB.flush();
        None
    }
}

/// Записать перевод в кэш + подрезать историю автора до MAX_CACHE_PER_AUTHOR.
fn cache_store(author: &str, post_id: &str, target: &str, text: &str, translated: &str) {
    let key = cache_key(author, post_id, target);
    let entry = CacheEntry {
        src_text_hash: content_hash(text),
        translated_text: translated.to_string(),
        timestamp: now_secs(),
    };
    if let Ok(bytes) = serde_json::to_vec(&entry) {
        let _ = DB.insert(key.as_bytes(), bytes);
        prune_author(author);
        let _ = DB.flush();
    }
}

/// Жёсткий лимит объёма кэша на одного автора: оставляем только
/// MAX_CACHE_PER_AUTHOR самых свежих записей (по timestamp), остальное стираем.
fn prune_author(author: &str) {
    let prefix = author_prefix(author);
    let mut items: Vec<(Vec<u8>, u64)> = Vec::new();
    for (k, v) in DB.scan_prefix(prefix.as_bytes()).flatten() {
        let ts = serde_json::from_slice::<CacheEntry>(&v)
            .map(|e| e.timestamp)
            .unwrap_or(0);
        items.push((k.to_vec(), ts));
    }
    if items.len() <= MAX_CACHE_PER_AUTHOR {
        return;
    }
    items.sort_by_key(|(_, ts)| *ts); // старые — в начало
    let remove_n = items.len() - MAX_CACHE_PER_AUTHOR;
    for (k, _) in items.into_iter().take(remove_n) {
        let _ = DB.remove(k);
    }
}

#[derive(Serialize, Clone)]
pub(crate) struct TranslationCacheStats {
    pub count: usize,
    pub bytes: u64,
    pub avg_bytes: u64,
}

/// Статистика кэша переводов: число записей, суммарный размер и средний размер.
#[tauri::command]
pub(crate) async fn translation_cache_stats() -> TranslationCacheStats {
    tauri::async_runtime::spawn_blocking(|| {
        let mut count = 0usize;
        let mut bytes = 0u64;
        for (k, v) in DB.scan_prefix(b"social:translation:").flatten() {
            count += 1;
            bytes += (k.len() + v.len()) as u64;
        }
        let avg_bytes = if count > 0 { bytes / count as u64 } else { 0 };
        TranslationCacheStats {
            count,
            bytes,
            avg_bytes,
        }
    })
    .await
    .unwrap_or(TranslationCacheStats {
        count: 0,
        bytes: 0,
        avg_bytes: 0,
    })
}

/// Полностью очистить кэш переводов (все записи `social:translation:*`).
/// Возвращает количество удалённых записей.
#[tauri::command]
pub(crate) async fn clear_translation_cache() -> Result<usize, String> {
    let removed = tauri::async_runtime::spawn_blocking(|| {
        let keys: Vec<Vec<u8>> = DB
            .scan_prefix(b"social:translation:")
            .flatten()
            .map(|(k, _)| k.to_vec())
            .collect();
        let n = keys.len();
        for k in keys {
            let _ = DB.remove(k);
        }
        let _ = DB.flush();
        n
    })
    .await
    .map_err(|e| e.to_string())?;
    crate::syslog::log(
        "sys",
        format!("AI translator: translation cache cleared ({removed} entries)"),
    );
    Ok(removed)
}


fn lang_name(code: &str) -> &str {
    match code {
        "ru" => "Russian",
        "en" => "English",
        "id" => "Indonesian",
        "uk" => "Ukrainian",
        "es" => "Spanish",
        "pt" => "Portuguese",
        "fr" => "French",
        "de" => "German",
        "it" => "Italian",
        "tr" => "Turkish",
        "pl" => "Polish",
        "nl" => "Dutch",
        "ar" => "Arabic",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        "hi" => "Hindi",
        _ => "English",
    }
}

/// Строгий Instruct-промпт (исключает галлюцинации/лишний текст).
/// Использует ChatMLQwen — Qwen2, Qwen2.5 и Qwen3 понимают один и тот же
/// шаблон `<|im_start|> ... <|im_end|>`.
fn build_prompt(text: &str, target_code: &str) -> String {
    match translator_config::config().chat_template {
        ChatTemplate::ChatMlQwen => format!(
            "<|im_start|>system\nYou are a precise translator. Translate the user text to {} directly. Do not add any explanations, notes, or commentary.<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\n",
            lang_name(target_code),
            text
        ),
    }
}

/// Ленивая инициализация модели (один раз). Диспетчер по движку.
/// Блокирующая — вызывать в spawn_blocking.
fn ensure_engine(app: &AppHandle) -> Result<(), String> {
    let cfg = translator_config::config();
    // Явный отказ для неподдерживаемых сборок/форматов (Bonsai Q1_0 без фичи bonsai).
    if !cfg.supported {
        return Err(format!(
            "Модель '{}' не поддерживается текущей сборкой. {}",
            cfg.name, cfg.unsupported_reason
        ));
    }
    if !is_installed(app) {
        // файл есть, но не проходит проверку → карантин + фоновая перекачка
        if model_path(app).exists() {
            if let Err(why) = gguf_sane(&model_path(app)) {
                quarantine_corrupt_model(app, &why);
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = translator_bootstrap(app2).await;
                });
            }
        }
        return Err("Языковой пакет не установлен".to_string());
    }
    match cfg.engine {
        EngineKind::Candle => ensure_engine_candle(app),
        EngineKind::LlamaCpp => {
            #[cfg(feature = "bonsai")]
            {
                llama_engine::ensure_loaded(app)
            }
            #[cfg(not(feature = "bonsai"))]
            {
                Err("Bonsai (Q1_0) требует сборку клиента с флагом --features bonsai".to_string())
            }
        }
    }
}

/// candle-путь: грузит квантованные веса Qwen2/Qwen3 в общий ENGINE.
fn ensure_engine_candle(app: &AppHandle) -> Result<(), String> {
    {
        let guard = ENGINE.lock().map_err(|_| "engine lock".to_string())?;
        if guard.is_some() {
            return Ok(());
        }
    }
    let cfg = translator_config::config();
    let device = select_device();
    // На GPU (CUDA/Metal) лимит CPU-потоков не нужен — инференс идёт на видеокарте.
    // На CPU сохраняем cap min(4, cpus/2), чтобы P2P-сидирование и UI не зависали.
    if ACCEL.lock().map(|g| g.as_str() == "cpu").unwrap_or(true) {
        apply_thread_limit();
    }
    let mp = model_path(app);
    let tp = tokenizer_path(app);

    let tokenizer = Tokenizer::from_file(&tp).map_err(|e| format!("токенизатор: {e}"))?;
    let mut file = std::fs::File::open(&mp).map_err(|e| format!("модель: {e}"))?;
    let content =
        gguf_file::Content::read(&mut file).map_err(|e| format!("GGUF-метаданные: {e}"))?;

    // Ветвление по архитектуре: Qwen2 vs Qwen3 (candle-модули различны).
    let model = match cfg.architecture {
        Architecture::Qwen2 => ModelBackend::Qwen2(
            candle_transformers::models::quantized_qwen2::ModelWeights::from_gguf(
                content,
                &mut file,
                &device,
            )
            .map_err(|e| format!("загрузка весов Qwen2: {e}"))?,
        ),
        Architecture::Qwen3 => ModelBackend::Qwen3(
            candle_transformers::models::quantized_qwen3::ModelWeights::from_gguf(
                content,
                &mut file,
                &device,
            )
            .map_err(|e| format!("загрузка весов Qwen3: {e}"))?,
        ),
    };

    let mut guard = ENGINE.lock().map_err(|_| "engine lock".to_string())?;
    *guard = Some(Engine {
        model,
        tokenizer,
        device,
    });
    Ok(())
}

/// Прогнать инференс перевода. Диспетчер по движку (candle / llama.cpp).
fn run_inference(text: &str, target_code: &str) -> Result<String, String> {
    match translator_config::config().engine {
        EngineKind::Candle => run_inference_candle(text, target_code),
        EngineKind::LlamaCpp => {
            #[cfg(feature = "bonsai")]
            {
                let prompt = build_prompt(text, target_code);
                llama_engine::run_inference(&prompt)
            }
            #[cfg(not(feature = "bonsai"))]
            {
                let _ = (text, target_code);
                Err("Bonsai (Q1_0) требует сборку клиента с флагом --features bonsai".to_string())
            }
        }
    }
}

/// candle-путь инференса (блокирующая часть).
fn run_inference_candle(text: &str, target_code: &str) -> Result<String, String> {
    let mut guard = ENGINE.lock().map_err(|_| "engine lock".to_string())?;
    let engine = guard.as_mut().ok_or("engine not initialized")?;
    // KV-кэш сбрасывается автоматически: первый forward каждого перевода идёт с
    // index_pos = 0, а candle при index_pos == 0 игнорирует прежний кэш слоёв.
    // (Отдельный clear_kv_cache() есть не во всех версиях candle, поэтому не зовём.)

    let prompt = build_prompt(text, target_code);
    let encoding = engine
        .tokenizer
        .encode(prompt, true)
        .map_err(|e| format!("токенизация: {e}"))?;
    let prompt_tokens: Vec<u32> = encoding.get_ids().to_vec();
    if prompt_tokens.is_empty() {
        return Err("пустой ввод".to_string());
    }

    let mut logits_processor = LogitsProcessor::new(299_792_458, Some(0.0), None);

    // Первый проход — весь промпт с позиции 0.
    let input = Tensor::new(prompt_tokens.as_slice(), &engine.device)
        .and_then(|t| t.unsqueeze(0))
        .map_err(|e| format!("тензор ввода: {e}"))?;
    let mut logits = engine
        .model
        .forward(&input, 0)
        .and_then(|l| l.squeeze(0))
        .map_err(|e| format!("forward: {e}"))?;

    let mut out_tokens: Vec<u32> = Vec::new();
    let mut pos = prompt_tokens.len();
    // На GPU разрешаем более длинную генерацию (быстро), на CPU — осторожный лимит.
    let max_new = if ACCEL.lock().map(|g| g.as_str() != "cpu").unwrap_or(false) {
        MAX_NEW_TOKENS_GPU
    } else {
        MAX_NEW_TOKENS
    };
    let eos = &translator_config::config().eos_tokens;
    for _ in 0..max_new {
        let next = logits_processor
            .sample(&logits)
            .map_err(|e| format!("sample: {e}"))?;
        if eos.iter().any(|&e| e == next) {
            break;
        }
        out_tokens.push(next);
        let input = Tensor::new(&[next], &engine.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| format!("тензор шага: {e}"))?;
        logits = engine
            .model
            .forward(&input, pos)
            .and_then(|l| l.squeeze(0))
            .map_err(|e| format!("forward: {e}"))?;
        pos += 1;
    }

    let text_out = engine
        .tokenizer
        .decode(&out_tokens, true)
        .map_err(|e| format!("декод: {e}"))?;
    Ok(text_out.trim().to_string())
}

/// Перевести текст поста/комментария на `target_lang`. Сначала — кэш Sled
/// (с проверкой хэша оригинала). `author` — владелец стены (для лимита на автора).
#[tauri::command]
pub(crate) async fn translate_text(
    app: AppHandle,
    author: String,
    post_id: String,
    text: String,
    target_lang: String,
) -> Result<String, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Пустой текст".to_string());
    }
    let target = if target_lang.trim().is_empty() {
        "en".to_string()
    } else {
        target_lang.trim().to_lowercase()
    };
    let author = author.trim().to_string();

    // 1) Кэш: перевод есть И хэш оригинала совпадает — мгновенно.
    if let Some(v) = cache_lookup(&author, &post_id, &target, &text) {
        return Ok(v);
    }

    // 2) Инференс в spawn_blocking (Main Thread свободен, P2P не морозится).
    let app2 = app.clone();
    let t2 = text.clone();
    let target2 = target.clone();
    let translated = tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        ensure_engine(&app2)?;
        run_inference(&t2, &target2)
    })
    .await
    .map_err(|e| e.to_string())??;

    // 3) Сохраняем в Sled-кэш (хэш + timestamp) + подрезаем историю автора.
    cache_store(&author, &post_id, &target, &text, &translated);
    Ok(translated)
}

// ============================================================

/// Ускоритель, с которым СОБРАН клиент (compile-time): "cuda" | "metal" | "cpu".
/// Перевод страниц DApp доступен только в GPU-сборках (иначе слишком медленно).
#[tauri::command]
pub(crate) fn translator_build_accel() -> &'static str {
    #[cfg(feature = "cuda")]
    {
        "cuda"
    }
    #[cfg(all(feature = "metal", not(feature = "cuda")))]
    {
        "metal"
    }
    #[cfg(all(not(feature = "cuda"), not(feature = "metal")))]
    {
        "cpu"
    }
}

/// Может ли ЭТА сборка реально переводить страницы DApp локально (а не только
/// показывать кэш из P2P). GPU-сборки — да (быстро). CPU-сборка — да, если
/// собрана с `bonsai`: 1-битный Q1_0 достаточно быстр на CPU для авто-перевода.
#[tauri::command]
pub(crate) fn translator_dapp_capable() -> bool {
    translator_build_accel() != "cpu" || cfg!(feature = "bonsai")
}

/// Человекочитаемая метка движка переводов для UI (Settings → Storage):
/// "GPU CUDA" | "GPU METAL" | "CPU + BONSAI" | "CPU".
#[tauri::command]
pub(crate) fn translator_engine_label() -> &'static str {
    match translator_build_accel() {
        "cuda" => "GPU CUDA",
        "metal" => "GPU METAL",
        _ if cfg!(feature = "bonsai") => "CPU + BONSAI",
        _ => "CPU",
    }
}

// ============================================================
//  Bonsai (Q1_0) через FFI к llama.cpp — feature `bonsai`.
//
//  candle не читает 1-битный GGUF-тип Q1_0, поэтому Bonsai грузится
//  крейтом `llama-cpp-2` (safe-биндинги к llama.cpp, Q1_0 в upstream).
//  Backend инициализируется один раз, модель живёт в статике; на каждый
//  перевод создаётся свежий контекст (чистый KV-кэш). Весь путь сериализован
//  и вызывается строго из spawn_blocking (Main Thread / P2P не морозятся).
// ============================================================
#[cfg(feature = "bonsai")]
mod llama_engine {
    use llama_cpp_2::context::params::LlamaContextParams;
    use llama_cpp_2::llama_backend::LlamaBackend;
    use llama_cpp_2::llama_batch::LlamaBatch;
    use llama_cpp_2::model::params::LlamaModelParams;
    use llama_cpp_2::model::{AddBos, LlamaModel};
    use llama_cpp_2::sampling::LlamaSampler;
    use once_cell::sync::Lazy;
    use std::num::NonZeroU32;
    use std::sync::Mutex;
    use tauri::AppHandle;

    /// Backend + загруженная модель. Держим вместе, чтобы контекст (который
    /// заимствует модель) создавался и жил строго внутри одного lock-скоупа.
    struct LlamaState {
        _backend: LlamaBackend,
        model: LlamaModel,
    }

    static STATE: Lazy<Mutex<Option<LlamaState>>> = Lazy::new(|| Mutex::new(None));

    /// Ленивая загрузка Bonsai GGUF (Q1_0). Идемпотентно.
    pub(super) fn ensure_loaded(app: &AppHandle) -> Result<(), String> {
        let mut guard = STATE.lock().map_err(|_| "llama lock".to_string())?;
        if guard.is_some() {
            return Ok(());
        }
        let backend =
            LlamaBackend::init().map_err(|e| format!("инициализация llama.cpp: {e}"))?;
        let mp = super::model_path(app);
        #[allow(unused_mut)]
        let mut params = LlamaModelParams::default();
        // При сборке с GPU-фичей выгружаем все слои на видеокарту.
        #[cfg(any(feature = "bonsai-cuda", feature = "bonsai-metal"))]
        {
            params = params.with_n_gpu_layers(1000);
            #[cfg(feature = "bonsai-cuda")]
            super::set_accel("cuda");
            #[cfg(feature = "bonsai-metal")]
            super::set_accel("metal");
        }
        let model = LlamaModel::load_from_file(&backend, &mp, &params)
            .map_err(|e| format!("загрузка Bonsai GGUF (Q1_0): {e}"))?;
        *guard = Some(LlamaState {
            _backend: backend,
            model,
        });
        Ok(())
    }

    /// Синхронный greedy-инференс по готовому ChatML-промпту.
    pub(super) fn run_inference(prompt: &str) -> Result<String, String> {
        let guard = STATE.lock().map_err(|_| "llama lock".to_string())?;
        let state = guard.as_ref().ok_or("Bonsai-модель не загружена")?;
        let model = &state.model;
        let backend = &state._backend;

        let threads = super::cpu_threads() as i32;
        let ctx_params = LlamaContextParams::default()
            .with_n_ctx(Some(NonZeroU32::new(4096).unwrap()))
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut ctx = model
            .new_context(backend, ctx_params)
            .map_err(|e| format!("llama context: {e}"))?;

        // llama.cpp разбирает спец-токены ChatML (`<|im_start|>` и т.п.) сам.
        let tokens = model
            .str_to_token(prompt, AddBos::Never)
            .map_err(|e| format!("токенизация: {e}"))?;
        if tokens.is_empty() {
            return Err("пустой ввод".to_string());
        }

        let mut batch = LlamaBatch::new(4096, 1);
        let last = tokens.len() - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch
                .add(*tok, i as i32, &[0], i == last)
                .map_err(|e| format!("batch add: {e}"))?;
        }
        ctx.decode(&mut batch).map_err(|e| format!("decode: {e}"))?;

        let mut sampler = LlamaSampler::greedy();
        let mut decoder = encoding_rs::UTF_8.new_decoder();
        let mut out = String::new();
        let mut n_cur = batch.n_tokens();
        // Явные EOS-id (как в candle-пути): страхуют, если GGUF не пометил
        // <|im_end|>/<|endoftext|> как EOG — иначе генерация не остановится и
        // будет молотить все max_new токенов на каждый перевод (клиент «висит»).
        let eos = crate::translator_config::config().eos_tokens;
        let max_new = if super::ACCEL.lock().map(|g| g.as_str() != "cpu").unwrap_or(false) {
            super::MAX_NEW_TOKENS_GPU
        } else {
            super::MAX_NEW_TOKENS
        };
        for _ in 0..max_new {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if model.is_eog_token(token) || eos.contains(&(token.0 as u32)) {
                break;
            }
            if let Ok(piece) = model.token_to_piece(token, &mut decoder, false, None) {
                out.push_str(&piece);
            }
            batch.clear();
            batch
                .add(token, n_cur, &[0], true)
                .map_err(|e| format!("batch add: {e}"))?;
            n_cur += 1;
            ctx.decode(&mut batch).map_err(|e| format!("decode: {e}"))?;
        }
        Ok(out.trim().to_string())
    }
}

/// Отбраковка «плохого» ответа модели (объяснение вместо перевода / раздувание
/// длины). В таких случаях оставляем оригинал, чтобы не ломать вёрстку DApp.
fn looks_bad_translation(src: &str, tr: &str) -> bool {
    let s = tr.trim();
    if s.is_empty() {
        return true;
    }
    let src_len = src.chars().count().max(1);
    let tr_len = s.chars().count();
    if tr_len > src_len * 4 + 40 {
        return true;
    }
    let low = s.to_lowercase();
    const BAD: [&str; 6] = [
        "translat",
        "переводится",
        "не имеет смысл",
        "in russian",
        "на русский",
        "как \"",
    ];
    BAD.iter().any(|p| low.contains(p))
}

//  Перевод страниц DApp (браузерный DOM-перевод)
//  Кэш по ХЭШУ контента: DApp обновился → хэш изменился → новый перевод.
//  Ключ: social:translation:dapp:<blake3(text)>:<lang>. Лимит — MAX_DAPP_CACHE.
// ============================================================

const MAX_DAPP_CACHE: usize = 5000;

#[derive(Serialize, Deserialize)]
struct DappCacheEntry {
    t: String,
    ts: u64,
}

fn dapp_cache_key(text_hash: &str, target: &str) -> String {
    format!("social:translation:dapp:{}:{}", text_hash, target)
}

fn dapp_cache_get(text_hash: &str, target: &str) -> Option<String> {
    let raw = DB.get(dapp_cache_key(text_hash, target).as_bytes()).ok().flatten()?;
    serde_json::from_slice::<DappCacheEntry>(&raw).ok().map(|e| e.t)
}

fn dapp_cache_put(text_hash: &str, target: &str, translated: &str) {
    let entry = DappCacheEntry {
        t: translated.to_string(),
        ts: now_secs(),
    };
    if let Ok(b) = serde_json::to_vec(&entry) {
        let _ = DB.insert(dapp_cache_key(text_hash, target).as_bytes(), b);
    }
}

fn prune_dapp_cache() {
    let mut items: Vec<(Vec<u8>, u64)> = Vec::new();
    for (k, v) in DB.scan_prefix(b"social:translation:dapp:").flatten() {
        let ts = serde_json::from_slice::<DappCacheEntry>(&v).map(|e| e.ts).unwrap_or(0);
        items.push((k.to_vec(), ts));
    }
    if items.len() <= MAX_DAPP_CACHE {
        return;
    }
    items.sort_by_key(|(_, ts)| *ts);
    let n = items.len() - MAX_DAPP_CACHE;
    for (k, _) in items.into_iter().take(n) {
        let _ = DB.remove(k);
    }
}

/// Перевести пакет строк DApp на `target`. Возвращает переводы в том же порядке.
/// Кэш по хэшу текста → повторное включение тумблера/повторные строки не гоняют
/// CPU/GPU. Дубликаты строк в пакете переводятся один раз.
async fn dapp_translate_batch(
    app: &AppHandle,
    texts: Vec<String>,
    target: String,
    force_inference: bool,
) -> Result<(Vec<String>, Vec<bool>), String> {
    let target = if target.trim().is_empty() {
        "en".to_string()
    } else {
        target.trim().to_lowercase()
    };
    // Фаза 3: подписываемся на общий языковой топик, чтобы принимать переводы от
    // GPU-узлов (и на CPU-сборке — наполнять кэш переводами из сети).
    tr_net::subscribe_lang(&target);
    // Локальный инференс DApp разрешён на GPU-сборках и на CPU-сборках с bonsai
    // (1-битный Q1_0 быстр на CPU). Иначе — только кэш из P2P. `force_inference`
    // (ручной перевод выделения) запускает модель в любом случае.
    let cache_only = !translator_dapp_capable() && !force_inference;
    let hashes: Vec<String> = texts.iter().map(|t| content_hash(t)).collect();
    let mut out: Vec<Option<String>> = (0..texts.len())
        .map(|i| dapp_cache_get(&hashes[i], &target))
        .collect();
    // какие строки были в кэше ДО инференса (для статистики «N% из кэша»)
    let from_cache: Vec<bool> = out.iter().map(|o| o.is_some()).collect();

    // уникальные промахи (по хэшу) — переводим один раз
    let mut misses: Vec<usize> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..texts.len() {
        if out[i].is_some() {
            continue;
        }
        if seen.insert(hashes[i].clone()) {
            misses.push(i);
        }
    }

    if !misses.is_empty() && !cache_only {
        let app2 = app.clone();
        let target2 = target.clone();
        let jobs: Vec<(usize, String)> = misses.iter().map(|&i| (i, texts[i].clone())).collect();
        let results = tauri::async_runtime::spawn_blocking(move || -> Result<Vec<(usize, String)>, String> {
            ensure_engine(&app2)?;
            let mut r = Vec::new();
            for (i, txt) in &jobs {
                // язык строки совпадает с целевым → не переводим (кэшируем оригинал)
                let lang = whatlang::detect(txt.trim())
                    .filter(|d| d.confidence() >= 0.5)
                    .map(|d| lang_to_2(d.lang()).to_string())
                    .unwrap_or_default();
                if !lang.is_empty() && lang == target2 {
                    r.push((*i, txt.clone()));
                } else {
                    let raw = run_inference(txt, &target2)?;
                    // объяснение/раздувание → оставляем оригинал (защита вёрстки DApp)
                    let safe = if looks_bad_translation(txt, &raw) { txt.clone() } else { raw };
                    r.push((*i, safe));
                }
            }
            Ok(r)
        })
        .await
        .map_err(|e| e.to_string())??;
        for (i, tr) in results {
            dapp_cache_put(&hashes[i], &target, &tr);
            // Фаза 3: GPU-узел вещает готовый перевод в топик `tr:<lang>`, чтобы
            // CPU-узлы получили его без запуска модели. Оригиналы (перевод == исходник)
            // не публикуем — экономим трафик.
            if tr != texts[i] {
                tr_net::publish(&target, &hashes[i], &tr, texts[i].chars().count());
            }
            out[i] = Some(tr);
        }
        prune_dapp_cache();
        let _ = DB.flush();
    }

    // финал: дубликаты-промахи подтягиваем из кэша (уже записаны); иначе — оригинал
    let final_out: Vec<String> = (0..texts.len())
        .map(|i| {
            out[i]
                .clone()
                .or_else(|| dapp_cache_get(&hashes[i], &target))
                .unwrap_or_else(|| texts[i].clone())
        })
        .collect();
    Ok((final_out, from_cache))
}

#[derive(Deserialize)]
struct DappTrReq {
    id: String,
    texts: Vec<String>,
    target: String,
    /// `true` — запрос инициирован пользователем вручную (контекстное меню
    /// «Перевести выделение»). На CPU-сборках отменяет режим `cache_only`,
    /// потому что пользователь готов подождать пару секунд ради ОДНОГО
    /// выделения — в отличие от авто-перевода всей страницы, где CPU
    /// затягивал бы UI на минуты.
    #[serde(default)]
    manual: bool,
}

/// Обработчик события `dapp-translate-request` от content-скрипта DApp:
/// переводит пакет строк и отвечает событием `dapp-translate-response`.
pub(crate) fn handle_dapp_translate_request(app: &AppHandle, payload: &str) {
    let req: DappTrReq = match serde_json::from_str(payload) {
        Ok(r) => r,
        Err(_) => return,
    };
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let (translations, from_cache) =
            dapp_translate_batch(&app2, req.texts, req.target, req.manual)
                .await
                .unwrap_or_default();
        let _ = app2.emit(
            "dapp-translate-response",
            serde_json::json!({ "id": req.id, "translations": translations, "from_cache": from_cache }),
        );
    });
}

// ============================================================
//  Фаза 3: P2P-шеринг кэша переводов (GPU → CPU)
//
//  GPU-узлы (CUDA/Metal) переводят строки DApp локальной моделью и вещают
//  пары (blake3-хэш оригинала, перевод) в общий gossip-топик по языку
//  `tr:<lang>`. CPU-узлы (без модели) слушают эти топики, валидируют пакет
//  (не пусто, длина ≤ 4× оригинала + 40, разумный максимум) и пишут перевод в
//  локальный Sled-кэш `social:translation:dapp:<hash>:<lang>` — так CPU-клиент
//  получает переводы «даром», не запуская тяжёлый инференс.
// ============================================================

#[cfg(feature = "p2p")]
pub(crate) mod tr_net {
    use super::{dapp_cache_key, now_secs, DappCacheEntry};
    use crate::DB;
    use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
    use iroh_gossip::{Gossip, TopicId};
    use once_cell::sync::Lazy;
    use serde::{Deserialize, Serialize};
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    use tauri::{AppHandle, Emitter};

    const MAX_PACKET: usize = 32 * 1024;
    const MAX_TR_CHARS: usize = 8192;

    static GOSSIP: Lazy<Mutex<Option<Gossip>>> = Lazy::new(|| Mutex::new(None));
    static APP: Lazy<Mutex<Option<AppHandle>>> = Lazy::new(|| Mutex::new(None));
    // lang -> живой отправитель топика `tr:<lang>`
    static SENDERS: Lazy<Mutex<HashMap<String, GossipSender>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    // языки, для которых подписка уже в процессе установки (защита от гонок)
    static SUBSCRIBING: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

    #[derive(Serialize, Deserialize)]
    struct TrMsg {
        h: String,   // blake3-хэш оригинала
        t: String,   // перевод
        l: String,   // язык-цель (ISO-639-1)
        n: usize,    // длина оригинала в символах (для валидации 4×)
    }

    fn topic_of(lang: &str) -> TopicId {
        let mut hh = blake3::Hasher::new();
        hh.update(b"smartnet-tr:v1:");
        hh.update(lang.as_bytes());
        TopicId::from_bytes(*hh.finalize().as_bytes())
    }

    /// Установить live-handle gossip (вызывается при поднятии endpoint).
    pub(crate) fn install(app: AppHandle, gossip: Gossip) {
        *APP.lock().unwrap() = Some(app);
        *GOSSIP.lock().unwrap() = Some(gossip);
    }

    fn app_handle() -> Option<AppHandle> {
        APP.lock().unwrap().clone()
    }

    /// Подписаться на топик языка (идемпотентно). Bootstrap — известные пиры сети.
    pub(crate) fn subscribe_lang(lang: &str) {
        let lang = lang.trim().to_lowercase();
        if lang.is_empty() {
            return;
        }
        let gossip = match GOSSIP.lock().unwrap().clone() {
            Some(g) => g,
            None => return,
        };
        {
            let mut subbing = SUBSCRIBING.lock().unwrap();
            if SENDERS.lock().unwrap().contains_key(&lang) || subbing.contains(&lang) {
                return;
            }
            subbing.insert(lang.clone());
        }
        let bootstrap: Vec<iroh::EndpointId> = crate::p2p::known_endpoint_ids()
            .into_iter()
            .filter_map(|s| s.parse::<iroh::EndpointId>().ok())
            .collect();
        let topic = topic_of(&lang);
        tauri::async_runtime::spawn(async move {
            let sub = match gossip.subscribe(topic, bootstrap).await {
                Ok(s) => s,
                Err(_) => {
                    SUBSCRIBING.lock().unwrap().remove(&lang);
                    return;
                }
            };
            let (tx, rx) = sub.split();
            SENDERS.lock().unwrap().insert(lang.clone(), tx);
            SUBSCRIBING.lock().unwrap().remove(&lang);
            pump(lang, rx).await;
        });
    }

    async fn pump(lang: String, mut rx: GossipReceiver) {
        use n0_future::StreamExt;
        while let Some(ev) = rx.next().await {
            match ev {
                Ok(Event::Received(m)) => {
                    if let crate::stealth::Incoming::Message(bytes) =
                        crate::stealth::parse_incoming(&m.content)
                    {
                        let json = String::from_utf8_lossy(&bytes).to_string();
                        tauri::async_runtime::spawn_blocking(move || ingest(&json));
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        SENDERS.lock().unwrap().remove(&lang);
    }

    /// Приём чужого перевода: базовая валидация + запись в локальный Sled-кэш DApp.
    fn ingest(json: &str) {
        let msg: TrMsg = match serde_json::from_str(json) {
            Ok(m) => m,
            Err(_) => return,
        };
        let lang = msg.l.trim().to_lowercase();
        // хэш — 64-символьный blake3 hex
        if lang.is_empty() || msg.h.len() != 64 || !msg.h.chars().all(|c| c.is_ascii_hexdigit()) {
            return;
        }
        let tr = msg.t.trim();
        let tr_len = tr.chars().count();
        // валидация: не пусто, разумный максимум, длина ≤ 4× оригинала + 40
        if tr.is_empty() || tr_len > MAX_TR_CHARS {
            return;
        }
        if msg.n > 0 && tr_len > msg.n * 4 + 40 {
            return;
        }
        let key = dapp_cache_key(&msg.h, &lang);
        // уже есть в кэше (в т.ч. свой локальный перевод) — не перезаписываем
        if DB.get(key.as_bytes()).ok().flatten().is_some() {
            return;
        }
        let entry = DappCacheEntry {
            t: tr.to_string(),
            ts: now_secs(),
        };
        if let Ok(b) = serde_json::to_vec(&entry) {
            let _ = DB.insert(key.as_bytes(), b);
            let _ = DB.flush();
        }
        if let Some(app) = app_handle() {
            let _ = app.emit(
                "translator-cache-updated",
                serde_json::json!({ "lang": lang }),
            );
        }
    }

    /// Опубликовать перевод в топик языка (вызывают только GPU-сборки).
    pub(crate) fn publish(lang: &str, hash: &str, translated: &str, orig_len: usize) {
        let lang = lang.trim().to_lowercase();
        if lang.is_empty() || hash.len() != 64 {
            return;
        }
        let msg = TrMsg {
            h: hash.to_string(),
            t: translated.to_string(),
            l: lang.clone(),
            n: orig_len,
        };
        let json = match serde_json::to_string(&msg) {
            Ok(j) => j,
            Err(_) => return,
        };
        if json.len() > MAX_PACKET {
            return;
        }
        subscribe_lang(&lang);
        crate::stealth::note_activity();
        let payload = crate::stealth::frame_outbound(json.into_bytes());
        tauri::async_runtime::spawn(async move {
            let sender = SENDERS.lock().unwrap().get(&lang).cloned();
            let tx = match sender {
                Some(t) => t,
                None => {
                    // подписка ещё устанавливается — подождём и попробуем снова
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    match SENDERS.lock().unwrap().get(&lang).cloned() {
                        Some(t) => t,
                        None => return,
                    }
                }
            };
            let _ = tx.broadcast(payload.into()).await;
        });
    }
}

// ---- Заглушка для сборки без P2P (десктоп собирается с --features p2p) ----
#[cfg(not(feature = "p2p"))]
pub(crate) mod tr_net {
    pub(crate) fn install(_app: tauri::AppHandle, _g: ()) {}
    pub(crate) fn subscribe_lang(_lang: &str) {}
    pub(crate) fn publish(_lang: &str, _hash: &str, _translated: &str, _orig_len: usize) {}
}



// ============================================================
//  Фоновый пред-перевод постов подписок (лента открывается уже готовой)
// ============================================================

#[derive(Serialize, Clone)]
struct PrewarmProgress {
    done: usize,
    finished: bool,
}

/// Перевести с кэшированием (блокирующая): из кэша (с проверкой хэша) или инференс + запись.
fn translate_and_cache(author: &str, post_id: &str, text: &str, target: &str) -> Result<String, String> {
    if let Some(v) = cache_lookup(author, post_id, target, text) {
        return Ok(v);
    }
    let out = run_inference(text, target)?;
    cache_store(author, post_id, target, text, &out);
    Ok(out)
}

/// Фоновый пред-перевод недавних постов всех подписок пользователя на `target_lang`.
/// Кладёт результат в Sled-кэш, чтобы лента открывалась уже переведённой мгновенно.
/// Тяжёлая работа — в spawn_blocking (лимит потоков сохраняется), не блокирует UI/P2P.
#[tauri::command]
pub(crate) async fn translate_prewarm(app: AppHandle, target_lang: String) -> Result<(), String> {
    if !is_installed(&app) {
        return Err("Языковой пакет не установлен".to_string());
    }
    let target = if target_lang.trim().is_empty() {
        "en".to_string()
    } else {
        target_lang.trim().to_lowercase()
    };
    if PREWARMING.swap(true, Ordering::SeqCst) {
        return Ok(()); // уже идёт
    }
    tauri::async_runtime::spawn(async move {
        let app3 = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            if ensure_engine(&app3).is_err() {
                return;
            }
            let mut done = 0usize;
            'outer: for author in crate::social::wall_follow_list() {
                for (pid, text) in crate::social::wall_post_texts(&author, 30) {
                    // уже в кэше (и не устарел) — пропускаем
                    if cache_lookup(&author, &pid, &target, &text).is_some() {
                        continue;
                    }
                    // переводим только «чужой» язык
                    let lang = whatlang::detect(text.trim())
                        .filter(|i| i.confidence() >= 0.5)
                        .map(|i| lang_to_2(i.lang()).to_string())
                        .unwrap_or_default();
                    if lang.is_empty() || lang == target {
                        continue;
                    }
                    if translate_and_cache(&author, &pid, &text, &target).is_ok() {
                        done += 1;
                        if done % 5 == 0 {
                            let _ = app3.emit(
                                "translator-prewarm",
                                PrewarmProgress {
                                    done,
                                    finished: false,
                                },
                            );
                        }
                    }
                    // разумный лимит за один прогон, чтобы не грузить CPU надолго
                    if done >= 80 {
                        break 'outer;
                    }
                }
            }
            let _ = app3.emit(
                "translator-prewarm",
                PrewarmProgress {
                    done,
                    finished: true,
                },
            );
        })
        .await;
        PREWARMING.store(false, Ordering::SeqCst);
    });
    Ok(())
}
