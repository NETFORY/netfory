// ============================================================
//  SmartNet // ai_client.rs
//
//  Клиент Netfory AI Compute для dApp-хука `window.smartholdem.useAI()`:
//  диспетчер трекера (GET /v1/ai/pick) выбирает лучшую AI-ноду →
//  iroh QUIC (ALPN "netfory/ai/1") → Infer → токены стримятся в dApp
//  событиями `dapp-ai-token` {streamId, t}; финал резолвит Promise
//  через dapp-response (см. dapp.rs, методы aiGenerate / aiCancel).
// ============================================================

#[cfg(feature = "p2p")]
use once_cell::sync::Lazy;
#[cfg(feature = "p2p")]
use std::collections::HashMap;
#[cfg(feature = "p2p")]
use std::sync::Mutex;
#[cfg(feature = "p2p")]
use tauri::Emitter;

#[cfg(feature = "p2p")]
const AI_ALPN: &[u8] = b"netfory/ai/1";
#[cfg(feature = "p2p")]
const FRAME_JSON: u8 = 0;
#[cfg(feature = "p2p")]
const MAX_FRAME: usize = 4 * 1024 * 1024;

/// streamId → (задача генерации, id ожидающего dApp-Promise)
#[cfg(feature = "p2p")]
static AI_TASKS: Lazy<Mutex<HashMap<String, (Option<tauri::async_runtime::JoinHandle<()>>, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Отменить генерацию: снять задачу и отклонить dApp-Promise.
#[cfg(feature = "p2p")]
pub(crate) fn cancel(app: &tauri::AppHandle, stream_id: &str) {
    if let Some((jh, req_id)) = AI_TASKS.lock().unwrap().remove(stream_id) {
        if let Some(jh) = jh {
            jh.abort();
        }
        crate::dapp::emit_dapp_response(app, &req_id, None, Some("useAI: cancelled".into()));
    }
}

/// Запустить стриминговую генерацию для dApp (aiGenerate).
#[cfg(feature = "p2p")]
pub(crate) fn spawn_generate(
    app: tauri::AppHandle,
    req_id: String,
    stream_id: String,
    prompt: String,
    model: Option<String>,
    max_tokens: usize,
    think: bool,
) {
    // регистрация ДО spawn — исключаем гонку «задача завершилась раньше insert»
    AI_TASKS.lock().unwrap().insert(stream_id.clone(), (None, req_id.clone()));
    let app2 = app.clone();
    let sid = stream_id.clone();
    let rid = req_id;
    let jh = tauri::async_runtime::spawn(async move {
        let res = generate(&app2, &sid, prompt, model, max_tokens, think).await;
        // если задачу уже сняли (cancel) — Promise уже отклонён
        if AI_TASKS.lock().unwrap().remove(&sid).is_none() {
            return;
        }
        match res {
            Ok(v) => crate::dapp::emit_dapp_response(&app2, &rid, Some(v), None),
            Err(e) => crate::dapp::emit_dapp_response(&app2, &rid, None, Some(e)),
        }
    });
    if let Some(entry) = AI_TASKS.lock().unwrap().get_mut(&stream_id) {
        entry.0 = Some(jh);
    }
}

/// Диспетчер: спросить AI-трекеры, кто сейчас лучший исполнитель.
#[cfg(feature = "p2p")]
async fn pick_node(model: Option<String>) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        for t in crate::p2p::get_trackers_internal() {
            let mut url = format!("{}/v1/ai/pick", crate::p2p::normalize_tracker(&t));
            if let Some(m) = &model {
                url.push_str(&format!("?model={m}"));
            }
            let Ok(resp) = ureq::get(&url).timeout(std::time::Duration::from_secs(8)).call() else {
                continue;
            };
            let Ok(txt) = resp.into_string() else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
            if let Some(nid) = v["irohNodeId"].as_str() {
                return Ok(nid.to_string());
            }
        }
        Err("AI-диспетчер: в сети нет подходящих AI-нод".to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(feature = "p2p")]
async fn write_frame(
    send: &mut iroh::endpoint::SendStream,
    kind: u8,
    data: &[u8],
) -> Result<(), String> {
    let mut buf = Vec::with_capacity(5 + data.len());
    buf.push(kind);
    buf.extend_from_slice(&(data.len() as u32).to_be_bytes());
    buf.extend_from_slice(data);
    send.write_all(&buf).await.map_err(|e| e.to_string())
}

#[cfg(feature = "p2p")]
async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<(u8, Vec<u8>), String> {
    let mut h = [0u8; 5];
    recv.read_exact(&mut h).await.map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
    if len > MAX_FRAME {
        return Err("кадр слишком большой".into());
    }
    let mut p = vec![0u8; len];
    recv.read_exact(&mut p).await.map_err(|e| e.to_string())?;
    Ok((h[0], p))
}

#[cfg(feature = "p2p")]
async fn generate(
    app: &tauri::AppHandle,
    stream_id: &str,
    prompt: String,
    model: Option<String>,
    max_tokens: usize,
    think: bool,
) -> Result<serde_json::Value, String> {
    let target = pick_node(model.clone()).await?;
    let id: iroh::EndpointId = target
        .parse()
        .map_err(|_| format!("плохой iroh NodeId от диспетчера: {target}"))?;
    let endpoint = crate::p2p::ai_iroh_endpoint(app).await?;
    // адреса AI-ноды из sled-книги пиров; при промахе — discovery
    let conn = crate::p2p::connect_known(&endpoint, id, AI_ALPN, std::time::Duration::from_secs(30))
        .await
        .map_err(|e| format!("connect ai://{target}: {e}"))?;
    crate::peerbook::remember(&endpoint, id, crate::peerbook::Role::Ai).await;
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    let req = serde_json::json!({
        "type": "infer",
        "prompt": prompt,
        "maxTokens": max_tokens,
        "model": model,
        "think": think,
    });
    write_frame(&mut send, FRAME_JSON, req.to_string().as_bytes()).await?;
    let _ = send.finish();
    crate::syslog::log("ai", format!("useAI → ai://{}…", &target[..16.min(target.len())]));
    // токены летят в dApp по мере генерации
    loop {
        let (kind, buf) = read_frame(&mut recv).await?;
        if kind != FRAME_JSON {
            continue;
        }
        let Ok(m) = serde_json::from_slice::<serde_json::Value>(&buf) else {
            continue;
        };
        match m["type"].as_str().unwrap_or("") {
            "token" => {
                let _ = app.emit(
                    "dapp-ai-token",
                    serde_json::json!({ "streamId": stream_id, "t": m["t"].as_str().unwrap_or("") }),
                );
            }
            "done" => {
                return Ok(serde_json::json!({
                    "text": m["text"].as_str().unwrap_or(""),
                    "node": m["node"].as_str().unwrap_or(""),
                    "ms": m["ms"].as_u64().unwrap_or(0),
                }));
            }
            "error" => {
                return Err(m["e"].as_str().unwrap_or("ошибка AI-ноды").to_string());
            }
            _ => {}
        }
    }
}

// ============================================================
//  AI Чат Консоль (sn://ai-computer, только адреса из .env DEVELOPERS)
//  Тот же транспорт, что и useAI: токены летят событием `dapp-ai-token`
//  {streamId, t}; команда резолвится финальным {text, node, ms}.
// ============================================================

/// streamId → sender отмены активной консольной генерации
#[cfg(feature = "p2p")]
static CONSOLE_CANCELS: Lazy<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Скачивания моделей строго по одному: два FsStore на одной папке = конфликт
/// блокировки БД, а автоочистка после экспорта не должна снести чужой блоб.
#[cfg(feature = "p2p")]
static MODEL_DL_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn ai_console_generate(
    app: tauri::AppHandle,
    stream_id: String,
    prompt: String,
    model: Option<String>,
    max_tokens: Option<u32>,
    think: Option<bool>,
) -> Result<serde_json::Value, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("пустой prompt".into());
    }
    let max = max_tokens.unwrap_or(2048).clamp(1, 2048) as usize;
    let (ctx_tx, ctx_rx) = tokio::sync::oneshot::channel::<()>();
    CONSOLE_CANCELS.lock().unwrap().insert(stream_id.clone(), ctx_tx);
    let res = tokio::select! {
        r = generate(&app, &stream_id, prompt, model.filter(|m| !m.trim().is_empty()), max, think.unwrap_or(true)) => r,
        _ = ctx_rx => Err("остановлено".to_string()),
    };
    CONSOLE_CANCELS.lock().unwrap().remove(&stream_id);
    res
}

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) fn ai_console_cancel(stream_id: String) {
    if let Some(tx) = CONSOLE_CANCELS.lock().unwrap().remove(&stream_id) {
        let _ = tx.send(());
    }
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn ai_console_generate(
    _stream_id: String,
    _prompt: String,
    _model: Option<String>,
    _max_tokens: Option<u32>,
    _think: Option<bool>,
) -> Result<serde_json::Value, String> {
    Err("AI-сеть доступна только в p2p-сборке".into())
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) fn ai_console_cancel(_stream_id: String) {}

// ---------- скачивание модели каталога (тикет iroh-blobs) ----------

/// Скачать GGUF-модель по тикету "<hash>@<nodeId>" в выбранный юзером файл
/// (нативный Save As — юзер укажет models_dir своего воркера). Прогресс —
/// событие "ai-model-dl" {alias, bytes}. Ok(None) = диалог отменён.
#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn ai_model_download(
    app: tauri::AppHandle,
    alias: String,
    ticket: String,
) -> Result<Option<String>, String> {
    use iroh_blobs::api::proto::BlobStatus;
    use iroh_blobs::store::fs::options::Options;
    use iroh_blobs::store::fs::FsStore;
    use iroh_blobs::Hash;
    use tauri::Manager;

    let (h_str, nid_str) = ticket
        .trim()
        .split_once('@')
        .ok_or("формат тикета: <hash>@<nodeId>")?;
    let hash: Hash = h_str.parse().map_err(|_| "плохой hash в тикете".to_string())?;
    let nid: iroh::EndpointId = nid_str.parse().map_err(|_| "плохой nodeId в тикете".to_string())?;

    // 1) куда сохранять — нативный диалог (в отдельном потоке, не блокируем runtime)
    let app_d = app.clone();
    let fname = format!("{alias}.gguf");
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        use tauri_plugin_dialog::DialogExt;
        app_d.dialog().file().set_file_name(&fname).blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(dst) = chosen.and_then(|fp| fp.into_path().ok()) else {
        return Ok(None); // отменено
    };

    // 2) blob-store кеша моделей в app-data (скачивания по одному)
    let _dl_guard = MODEL_DL_LOCK.lock().await;
    let store_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("ai-models-store");
    std::fs::create_dir_all(&store_dir).map_err(|e| e.to_string())?;
    let endpoint = crate::p2p::ai_iroh_endpoint(&app).await?;
    let store = FsStore::load_with_opts(store_dir.join("blobs.db"), Options::new(&store_dir))
        .await
        .map_err(|e| format!("blob store: {e}"))?;

    // 3) поллер прогресса: сколько байт уже лежит в store
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let store = store.clone();
        let app_p = app.clone();
        let alias_p = alias.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(1500)) => {
                        if let Ok(BlobStatus::Partial { size }) = store.blobs().status(hash).await {
                            let _ = app_p.emit("ai-model-dl", serde_json::json!({
                                "alias": alias_p,
                                "bytes": size.unwrap_or(0),
                            }));
                        }
                    }
                }
            }
        });
    }

    // 4) до 5 попыток; истина — локальный статус блоба, не результат download()
    let mut complete = matches!(store.blobs().status(hash).await, Ok(BlobStatus::Complete { .. }));
    let mut last = String::from("провайдер не отдал данные");
    for _ in 0..5u32 {
        if complete {
            break;
        }
        if let Err(e) = store.downloader(&endpoint).download(hash, Some(nid)).await {
            last = e.to_string();
        }
        match store.blobs().status(hash).await {
            Ok(BlobStatus::Complete { .. }) => complete = true,
            Ok(BlobStatus::Partial { size }) => last = format!("частично: {} байт", size.unwrap_or(0)),
            Ok(BlobStatus::NotFound) => last = "у провайдера нет такого блоба".into(),
            Err(e) => last = format!("статус: {e}"),
        }
        if !complete {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    }
    let _ = stop_tx.send(());
    if !complete {
        return Err(format!("не удалось скачать: {last}"));
    }
    store
        .blobs()
        .export(hash, &dst)
        .await
        .map_err(|e| format!("экспорт: {e}"))?;
    // автоочистка: кеш нужен был только на время скачивания — освобождаем
    // место сразу после успешного экспорта (при неудаче partial-блоб
    // остаётся, чтобы повторная попытка докачала с того же места)
    drop(store);
    for attempt in 0..3u32 {
        tokio::time::sleep(std::time::Duration::from_millis(300 * (attempt as u64 + 1))).await;
        match std::fs::remove_dir_all(&store_dir) {
            Ok(()) => {
                crate::syslog::log("ai", "кеш ai-models-store очищен после экспорта".to_string());
                break;
            }
            Err(e) if attempt == 2 => {
                crate::syslog::log("ai", format!("кеш моделей не удалился ({e}) — очистите в Настройках"));
            }
            Err(_) => {}
        }
    }
    Ok(Some(dst.to_string_lossy().to_string()))
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn ai_model_download(
    _alias: String,
    _ticket: String,
) -> Result<Option<String>, String> {
    Err("AI-сеть доступна только в p2p-сборке".into())
}
