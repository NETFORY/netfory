// ============================================================
//  netdrop-ui // mailbox.rs — «почта доверенных» через облако.
//
//  У отправителя на его диске: .netdrop/mailbox/<recip>/ с шифрованным
//  index.json (реестр посылок, ключ пары) и чанками ≤100 МиБ
//  (per-transfer ключ лежит внутри index). Настройки ящика (ссылка +
//  ключ) передаются доверенному устройству один раз по P2P-каналу
//  (TextPush "ndmx1:…"). Получатель проверяет ящик раз в
//  NETDROP_MAILBOX_POLL_MIN минут (.env при сборке, деф. 20) или по
//  кнопке: Yandex/Google — анонимно, Mail.ru — только с тем же
//  аккаунтом. Найденное скачивается автоматически (с докачкой);
//  подтверждение «получено» повторяется раз в 10 минут до успеха,
//  отправитель после него авто-удаляет посылку из облака.
// ============================================================

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

use crate::cloud::{
    self, build_tar, cancel_flag, chunk_size, g_anyone_reader, g_create_folder, g_delete,
    g_find_folder, g_update_media, g_upload, mr_delete, mr_get, mr_mkcol, mr_put, mr_split,
    retry, seal_chunk, ya_delete, ya_mkdir, ya_publish, ya_public_download, ya_upload,
    ChunkSource, B64,
};

/// Интервал фоновой проверки почты, минуты (.env при сборке).
pub(crate) fn poll_minutes() -> u64 {
    option_env!("NETDROP_MAILBOX_POLL_MIN")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(20)
}

// ---------- модели ----------

#[derive(Serialize, Deserialize, Clone)]
struct MboxTransfer {
    tid: String,
    name: String,
    size: u64,
    chunks: u32,
    #[serde(default)]
    tar: bool,
    /// per-transfer ключ E2E (base64url, 32 байта)
    key: String,
    /// file id чанков (Google Drive)
    #[serde(default)]
    ids: Vec<String>,
    /// id папки посылки (Google Drive, для удаления)
    #[serde(default)]
    gfolder: String,
    ts: u64,
    #[serde(default)]
    confirmed: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct MboxIndex {
    v: u8,
    transfers: Vec<MboxTransfer>,
}

// ---------- шифрование index.json (ключ пары) ----------

fn seal_index(key: &[u8; 32], idx: &MboxIndex) -> Result<Vec<u8>, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    use rand::RngCore;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(idx).map_err(|e| e.to_string())?;
    let ct = ChaCha20Poly1305::new(Key::from_slice(key))
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: b"ndmbox" })
        .map_err(|_| "index encryption failed".to_string())?;
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn open_index(key: &[u8; 32], data: &[u8]) -> Result<MboxIndex, String> {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
    if data.len() < 13 {
        return Err("mailbox index truncated".into());
    }
    let (nonce, ct) = data.split_at(12);
    let plain = ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: b"ndmbox" })
        .map_err(|_| "mailbox index: wrong pair key or corrupted".to_string())?;
    serde_json::from_slice(&plain).map_err(|e| e.to_string())
}

fn key_from_b64(s: &str) -> Result<[u8; 32], String> {
    let raw = B64.decode(s.trim()).map_err(|_| "bad key")?;
    if raw.len() != 32 {
        return Err("bad key length".into());
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&raw);
    Ok(k)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Имя папки получателя на диске отправителя: первые 16 hex его node id.
fn recip_of(device_id: &str) -> String {
    device_id.chars().take(16).collect()
}

// ---------- хранилище (app data) ----------

fn outbox_read(app: &AppHandle) -> serde_json::Value {
    crate::data_file(app, "mailbox_out.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

fn outbox_write(app: &AppHandle, v: &serde_json::Value) {
    if let Some(p) = crate::data_file(app, "mailbox_out.json") {
        let _ = std::fs::write(p, v.to_string());
    }
}

fn seen_read(app: &AppHandle) -> serde_json::Value {
    crate::data_file(app, "mailbox_seen.json")
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "seen": {}, "receipts": [] }))
}

fn seen_write(app: &AppHandle, v: &serde_json::Value) {
    if let Some(p) = crate::data_file(app, "mailbox_seen.json") {
        let _ = std::fs::write(p, v.to_string());
    }
}

// ---------- управление по P2P-каналу (TextPush "ndmx1:…") ----------

/// true — сообщение служебное (обработано), в буфер обмена не отдаём.
pub(crate) fn handle_control(app: &AppHandle, peer_hex: &str, text: &str) -> bool {
    let Some(b64) = text.strip_prefix("ndmx1:") else { return false };
    let Ok(raw) = B64.decode(b64.trim()) else { return true };
    let Ok(j) = serde_json::from_slice::<serde_json::Value>(&raw) else { return true };
    match j["t"].as_str() {
        // объявление ящика: доверенный сообщает, где забирать его посылки
        Some("mbox") => {
            let mut list = crate::load_paired(app);
            let mut ok = false;
            for d in list.iter_mut() {
                if d["id"].as_str() == Some(peer_hex) {
                    d["mbox"] = json!({
                        "provider": j["provider"], "url": j["url"],
                        "idx": j["idx"], "key": j["key"], "acct": j["acct"],
                    });
                    ok = true;
                }
            }
            if ok {
                crate::save_paired(app, &list);
                crate::emit(app, json!({ "kind": "mbox-announce", "id": peer_hex }));
            }
        }
        // подтверждение получения → фронтенд удалит посылку из облака
        Some("got") => {
            let Some(tid) = j["tid"].as_str() else { return true };
            let mut ob = outbox_read(app);
            let mut info: Option<(String, String)> = None;
            if let Some(e) = ob.get_mut(peer_hex) {
                let provider = e["provider"].as_str().unwrap_or("").to_string();
                let account = e["account"].as_str().unwrap_or("").to_string();
                if let Some(arr) = e["transfers"].as_array_mut() {
                    for t in arr.iter_mut() {
                        if t["tid"].as_str() == Some(tid)
                            && !t["confirmed"].as_bool().unwrap_or(false)
                        {
                            t["confirmed"] = json!(true);
                            info = Some((provider.clone(), account.clone()));
                        }
                    }
                }
            }
            if let Some((provider, account)) = info {
                outbox_write(app, &ob);
                crate::emit(app, json!({
                    "kind": "mbox-confirmed", "device": peer_hex, "tid": tid,
                    "provider": provider, "account": account,
                }));
            }
        }
        _ => {}
    }
    true
}

// ---------- отправка ----------

#[allow(clippy::too_many_arguments)]
async fn send_inner(
    provider: &str,
    token: &str,
    entry: &serde_json::Value,
    recip: &str,
    tid: &str,
    key: &[u8; 32],
    src: &Path,
    size: u64,
    chunks: u32,
    counter: &AtomicU64,
    cancelled: &AtomicBool,
    ids: &mut Vec<String>,
    gfolder: &mut String,
) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    match provider {
        "yandex" => {
            ya_mkdir(token, &format!("disk:/.netdrop/mailbox/{recip}/{tid}")).await?;
        }
        "gdrive" => {
            let gmbox = entry["gmbox"].as_str().unwrap_or("");
            if gmbox.is_empty() {
                return Err("mailbox folder id missing — re-run setup".into());
            }
            *gfolder = g_create_folder(token, tid, Some(gmbox)).await?;
        }
        "mailru" => {
            let (l, p) = mr_split(token)?;
            mr_mkcol(l, p, &format!("/.netdrop/mailbox/{recip}/{tid}")).await?;
        }
        other => return Err(format!("unknown provider '{other}'")),
    }
    let gf = gfolder.clone();
    let mut f = tokio::fs::File::open(src).await.map_err(|e| e.to_string())?;
    let csz = chunk_size() as u64;
    for i in 0..chunks as u64 {
        if cancelled.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let want = std::cmp::min(csz, size - i * csz) as usize;
        let mut buf = vec![0u8; want];
        f.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
        let (k, t) = (*key, tid.to_string());
        let enc = tauri::async_runtime::spawn_blocking(move || seal_chunk(&k, &t, i, &buf))
            .await
            .map_err(|e| e.to_string())??;
        let data = bytes::Bytes::from(enc);
        let cname = format!("c{i:04}");
        match provider {
            "yandex" => {
                let p = format!("disk:/.netdrop/mailbox/{recip}/{tid}/{cname}");
                retry(|| ya_upload(token, &p, data.clone())).await?;
            }
            "gdrive" => {
                let fid = retry(|| g_upload(token, &gf, &cname, data.clone())).await?;
                ids.push(fid);
            }
            "mailru" => {
                let (l, pw) = mr_split(token)?;
                let p = format!("/.netdrop/mailbox/{recip}/{tid}/{cname}");
                retry(|| mr_put(l, pw, &p, data.clone())).await?;
            }
            _ => unreachable!(),
        }
        counter.fetch_add(want as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// Перезаливка шифрованного index.json (только неподтверждённые посылки).
async fn upload_index(
    provider: &str,
    token: &str,
    entry: &serde_json::Value,
    recip: &str,
    pair_key: &[u8; 32],
    transfers: &[MboxTransfer],
) -> Result<(), String> {
    let active: Vec<MboxTransfer> = transfers.iter().filter(|t| !t.confirmed).cloned().collect();
    let data = bytes::Bytes::from(seal_index(pair_key, &MboxIndex { v: 1, transfers: active })?);
    match provider {
        "yandex" => {
            let p = format!("disk:/.netdrop/mailbox/{recip}/index.json");
            retry(|| ya_upload(token, &p, data.clone())).await
        }
        "gdrive" => {
            let idx = entry["idx"].as_str().unwrap_or("");
            if idx.is_empty() {
                return Err("index file id missing — re-run setup".into());
            }
            retry(|| g_update_media(token, idx, data.clone())).await
        }
        "mailru" => {
            let (l, p) = mr_split(token)?;
            let path = format!("/.netdrop/mailbox/{recip}/index.json");
            retry(|| mr_put(l, p, &path, data.clone())).await
        }
        other => Err(format!("unknown provider '{other}'")),
    }
}

/// Создать/обновить ящик для устройства + отправить ему настройки по P2P.
#[tauri::command]
pub(crate) async fn mailbox_setup(
    app: AppHandle,
    device_id: String,
    provider: String,
    token: String,
    account: String,
    login: String,
) -> Result<serde_json::Value, String> {
    let recip = recip_of(&device_id);
    let mut ob = outbox_read(&app);
    let existing = ob.get(device_id.as_str()).cloned();
    // ключ пары и облачные объекты переиспользуем при повторной настройке
    let (key_b64, mut url, mut idx, mut gmbox) = match &existing {
        Some(e) if e["provider"].as_str() == Some(provider.as_str()) => (
            e["key"].as_str().unwrap_or("").to_string(),
            e["url"].as_str().unwrap_or("").to_string(),
            e["idx"].as_str().unwrap_or("").to_string(),
            e["gmbox"].as_str().unwrap_or("").to_string(),
        ),
        _ => {
            // ключ из QR сопряжения (если показывали QR с облаком) или новый
            let k = qr_key_take(&app).unwrap_or_else(|| {
                use rand::RngCore;
                let mut k = [0u8; 32];
                rand::thread_rng().fill_bytes(&mut k);
                B64.encode(k)
            });
            (k, String::new(), String::new(), String::new())
        }
    };
    let key = key_from_b64(&key_b64)?;
    let transfers: Vec<MboxTransfer> = existing
        .as_ref()
        .and_then(|e| serde_json::from_value(e["transfers"].clone()).ok())
        .unwrap_or_default();
    let active: Vec<MboxTransfer> = transfers.iter().filter(|t| !t.confirmed).cloned().collect();
    let data = bytes::Bytes::from(seal_index(&key, &MboxIndex { v: 1, transfers: active })?);

    match provider.as_str() {
        "yandex" => {
            ya_mkdir(&token, "disk:/.netdrop").await?;
            ya_mkdir(&token, "disk:/.netdrop/mailbox").await?;
            let dir = format!("disk:/.netdrop/mailbox/{recip}");
            ya_mkdir(&token, &dir).await?;
            let ipath = format!("{dir}/index.json");
            retry(|| ya_upload(&token, &ipath, data.clone())).await?;
            url = ya_publish(&token, &dir).await?;
        }
        "gdrive" => {
            let root = match g_find_folder(&token, ".netdrop", None).await? {
                Some(id) => id,
                None => g_create_folder(&token, ".netdrop", None).await?,
            };
            let mb = match g_find_folder(&token, "mailbox", Some(&root)).await? {
                Some(id) => id,
                None => g_create_folder(&token, "mailbox", Some(&root)).await?,
            };
            if gmbox.is_empty() {
                gmbox = match g_find_folder(&token, &recip, Some(&mb)).await? {
                    Some(id) => id,
                    None => g_create_folder(&token, &recip, Some(&mb)).await?,
                };
            }
            g_anyone_reader(&token, &gmbox).await?;
            if idx.is_empty() {
                idx = retry(|| g_upload(&token, &gmbox, "index.json", data.clone())).await?;
            } else {
                retry(|| g_update_media(&token, &idx, data.clone())).await?;
            }
        }
        "mailru" => {
            let (l, p) = mr_split(&token)?;
            mr_mkcol(l, p, "/.netdrop").await?;
            mr_mkcol(l, p, "/.netdrop/mailbox").await?;
            let dir = format!("/.netdrop/mailbox/{recip}");
            mr_mkcol(l, p, &dir).await?;
            let ipath = format!("{dir}/index.json");
            retry(|| mr_put(l, p, &ipath, data.clone())).await?;
            url = dir;
        }
        other => return Err(format!("unknown provider '{other}'")),
    }

    // объявление ящика доверенному (он должен быть онлайн и слушать)
    let announce = json!({
        "t": "mbox", "provider": provider, "url": url, "idx": idx,
        "key": key_b64, "acct": login,
    });
    let msg = format!("ndmx1:{}", B64.encode(announce.to_string()));
    crate::send_text_to_device(app.clone(), device_id.clone(), msg)
        .await
        .map_err(|e| format!("mailbox created, but the device is unreachable — retry setup when it is online ({e})"))?;

    ob[device_id.as_str()] = json!({
        "device": device_id, "provider": provider, "account": account, "login": login,
        "url": url, "idx": idx, "gmbox": gmbox, "key": key_b64,
        "transfers": transfers,
    });
    outbox_write(&app, &ob);
    Ok(json!({ "ok": true }))
}

/// Отправка файлов доверенному устройству через его облачный ящик.
#[tauri::command]
pub(crate) async fn mailbox_send(
    app: AppHandle,
    state: State<'_, crate::UiState>,
    id: u64,
    device_id: String,
    token: String,
    paths: Vec<String>,
) -> Result<serde_json::Value, String> {
    let ob0 = outbox_read(&app);
    let entry = ob0
        .get(device_id.as_str())
        .cloned()
        .ok_or("mailbox is not set up for this device")?;
    let provider = entry["provider"].as_str().unwrap_or("").to_string();
    let recip = recip_of(&device_id);
    if paths.is_empty() {
        return Err("no paths selected".to_string());
    }
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    for p in &paths {
        if !tokio::fs::try_exists(p).await.unwrap_or(false) {
            return Err(format!("path not found: {}", p.display()));
        }
    }
    let single_file = paths.len() == 1
        && tokio::fs::metadata(&paths[0]).await.map(|m| m.is_file()).unwrap_or(false);
    let name = crate::bundle_name(&paths);
    let (src_path, tar_mode, tmp) = if single_file {
        (paths[0].clone(), false, None)
    } else {
        let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&cache).await.ok();
        let tmp = cache.join(format!("ndmbox-{id}.tar"));
        let (t2, ps) = (tmp.clone(), paths.clone());
        tauri::async_runtime::spawn_blocking(move || build_tar(&ps, &t2))
            .await
            .map_err(|e| e.to_string())??;
        (tmp.clone(), true, Some(tmp))
    };
    let size = tokio::fs::metadata(&src_path).await.map_err(|e| e.to_string())?.len();
    let chunks = size.div_ceil(chunk_size() as u64).max(1) as u32;
    let (tid, tkey) = {
        use rand::RngCore;
        let mut b = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut b);
        let tid: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let mut k = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut k);
        (tid, k)
    };

    let cancelled = cancel_flag(&app, id).await;
    crate::track_active(&app, id, format!("☁ {name}"));
    let counter = Arc::new(AtomicU64::new(0));
    let watcher = crate::watch_progress(app.clone(), id, counter.clone(), size);
    crate::emit(&app, json!({ "id": id, "kind": "accepted" }));

    let mut ids: Vec<String> = Vec::new();
    let mut gfolder = String::new();
    let res = send_inner(
        &provider, &token, &entry, &recip, &tid, &tkey, &src_path, size, chunks,
        &counter, &cancelled, &mut ids, &mut gfolder,
    )
    .await;

    watcher.abort();
    crate::untrack_active(&app, id);
    state.cancels.lock().await.remove(&id);
    if let Some(t) = tmp {
        let _ = tokio::fs::remove_file(t).await;
    }

    match res {
        Ok(()) => {
            let tr = MboxTransfer {
                tid: tid.clone(), name: name.clone(), size, chunks, tar: tar_mode,
                key: B64.encode(tkey), ids, gfolder, ts: now_ms(), confirmed: false,
            };
            let mut ob = outbox_read(&app);
            let pair_key;
            let transfers: Vec<MboxTransfer>;
            {
                let e = ob.get_mut(device_id.as_str()).ok_or("outbox disappeared")?;
                pair_key = key_from_b64(e["key"].as_str().unwrap_or(""))?;
                let mut list: Vec<MboxTransfer> =
                    serde_json::from_value(e["transfers"].clone()).unwrap_or_default();
                list.push(tr);
                e["transfers"] = serde_json::to_value(&list).map_err(|x| x.to_string())?;
                transfers = list;
            }
            upload_index(&provider, &token, &entry, &recip, &pair_key, &transfers).await?;
            outbox_write(&app, &ob);
            crate::emit(&app, json!({ "id": id, "kind": "done", "bytes": size, "total": size, "path": "" }));
            Ok(json!({ "ok": true, "tid": tid, "name": name, "size": size, "chunks": chunks }))
        }
        Err(e) => {
            // подчищаем недокачанную посылку в облаке
            let _ = delete_parcel(&provider, &token, &recip, &tid, &gfolder).await;
            if cancelled.load(Ordering::Relaxed) {
                crate::emit(&app, json!({ "id": id, "kind": "cancelled" }));
                Err("cancelled".into())
            } else {
                crate::emit(&app, json!({ "id": id, "kind": "error", "message": e.clone() }));
                Err(e)
            }
        }
    }
}

async fn delete_parcel(
    provider: &str,
    token: &str,
    recip: &str,
    tid: &str,
    gfolder: &str,
) -> Result<(), String> {
    match provider {
        "yandex" => ya_delete(token, &format!("disk:/.netdrop/mailbox/{recip}/{tid}")).await,
        "gdrive" => {
            if gfolder.is_empty() {
                Ok(())
            } else {
                g_delete(token, gfolder).await
            }
        }
        "mailru" => {
            let (l, p) = mr_split(token)?;
            mr_delete(l, p, &format!("/.netdrop/mailbox/{recip}/{tid}")).await
        }
        _ => Ok(()),
    }
}

/// Получатель подтвердил приём → удаляем посылку из облака и из index.
#[tauri::command]
pub(crate) async fn mailbox_finalize(
    app: AppHandle,
    device_id: String,
    tid: String,
    token: String,
) -> Result<(), String> {
    let mut ob = outbox_read(&app);
    let entry = ob.get(device_id.as_str()).cloned().ok_or("no outbox for device")?;
    let provider = entry["provider"].as_str().unwrap_or("").to_string();
    let recip = recip_of(&device_id);
    let mut transfers: Vec<MboxTransfer> =
        serde_json::from_value(entry["transfers"].clone()).unwrap_or_default();
    let Some(tr) = transfers.iter().find(|t| t.tid == tid).cloned() else {
        return Ok(());
    };
    delete_parcel(&provider, &token, &recip, &tid, &tr.gfolder).await?;
    transfers.retain(|t| t.tid != tid);
    let pair_key = key_from_b64(entry["key"].as_str().unwrap_or(""))?;
    upload_index(&provider, &token, &entry, &recip, &pair_key, &transfers).await?;
    ob[device_id.as_str()]["transfers"] =
        serde_json::to_value(&transfers).map_err(|e| e.to_string())?;
    outbox_write(&app, &ob);
    crate::emit(&app, json!({ "kind": "mbox-finalized", "device": device_id, "tid": tid }));
    Ok(())
}

// ---------- приём (поллинг входящих) ----------

/// Проверка всех входящих ящиков. Возвращает число новых посылок.
pub(crate) async fn poll_inboxes(app: &AppHandle, mailru_token: Option<&str>) -> u32 {
    flush_receipts(app).await;
    let paired = crate::load_paired(app);
    let mut found = 0u32;
    for d in paired {
        if d["mbox"].is_null() {
            continue;
        }
        let peer = d["id"].as_str().unwrap_or("").to_string();
        let peer_name = d["name"].as_str().unwrap_or("device").to_string();
        match poll_one(app, &peer, &peer_name, &d["mbox"], mailru_token).await {
            Ok(n) => found += n,
            Err(e) => eprintln!("netdrop mailbox: poll {peer_name}: {e}"),
        }
    }
    flush_receipts(app).await;
    // системное уведомление о новых посылках (гасится, если окно в фокусе)
    if found > 0 {
        crate::notify_if_unfocused(
            app,
            "netdrop — Cloud",
            &format!("Trusted mail: new files downloaded: {found}"),
        );
    }
    found
}

async fn poll_one(
    app: &AppHandle,
    peer: &str,
    peer_name: &str,
    mb: &serde_json::Value,
    mailru_token: Option<&str>,
) -> Result<u32, String> {
    let provider = mb["provider"].as_str().unwrap_or("");
    let key = key_from_b64(mb["key"].as_str().unwrap_or(""))?;
    let url = mb["url"].as_str().unwrap_or("").to_string();
    let idx_id = mb["idx"].as_str().unwrap_or("").to_string();
    // Mail.ru: только с кредами того же аккаунта (кнопка на странице Cloud)
    let creds = match provider {
        "mailru" => match mailru_token {
            Some(t) => t.to_string(),
            None => return Ok(0),
        },
        _ => String::new(),
    };
    // подпапка получателя (mbox из QR: url ведёт на корень mailbox владельца)
    let sub = mb["sub"].as_str().unwrap_or("");
    let subp = if sub.is_empty() { String::new() } else { format!("/{sub}") };
    let raw = match provider {
        "yandex" => {
            let ipath = format!("{subp}/index.json");
            retry(|| ya_public_download(&url, &ipath)).await?
        }
        "gdrive" => retry(|| cloud::g_public_download(&idx_id)).await?,
        "mailru" => {
            let (l, p) = mr_split(&creds)?;
            let path = format!("{url}{subp}/index.json");
            retry(|| mr_get(l, p, &path)).await?
        }
        other => return Err(format!("unknown provider '{other}'")),
    };
    let index = open_index(&key, &raw)?;
    if index.transfers.is_empty() {
        return Ok(0);
    }
    // папка загрузок — как у push-приёма от доверенных
    let cfg = crate::load_device_cfg(app);
    let mut od = PathBuf::from(cfg["outDir"].as_str().unwrap_or("").trim());
    if od.as_os_str().is_empty()
        || od == Path::new(".")
        || od == Path::new("/")
        || tokio::fs::create_dir_all(&od).await.is_err()
    {
        od = PathBuf::from(crate::default_out_dir(app.clone()));
    }
    tokio::fs::create_dir_all(&od).await.ok();

    let mut found = 0u32;
    for tr in index.transfers {
        {
            let seen = seen_read(app);
            if !seen["seen"][tr.tid.as_str()].is_null() {
                continue;
            }
        }
        let tkey = key_from_b64(&tr.key)?;
        let src = match provider {
            "yandex" => ChunkSource::Yandex { url: url.clone(), prefix: format!("{subp}/{}", tr.tid) },
            "gdrive" => ChunkSource::Gdrive { ids: tr.ids.clone() },
            "mailru" => ChunkSource::Mailru { creds: creds.clone(), base: format!("{url}{subp}/{}", tr.tid) },
            _ => unreachable!(),
        };
        let id = {
            let st = app.state::<crate::UiState>();
            st.next_id.fetch_add(1, Ordering::Relaxed)
        };
        let cancelled = cancel_flag(app, id).await;
        crate::emit(app, json!({ "id": id, "kind": "recv-start",
            "name": tr.name, "size": tr.size, "peer": peer_name }));
        crate::track_active(app, id, format!("☁↓ {} ← {peer_name}", tr.name));
        let counter = Arc::new(AtomicU64::new(0));
        let watcher = crate::watch_progress(app.clone(), id, counter.clone(), tr.size);
        let res = cloud::download_encrypted(
            app, id, &src, &tr.tid, &tkey, &tr.name, tr.chunks, tr.tar, &od, &counter, &cancelled,
        )
        .await;
        watcher.abort();
        {
            let st = app.state::<crate::UiState>();
            st.cancels.lock().await.remove(&id);
        }
        match res {
            Ok(path) => {
                crate::emit(app, json!({ "id": id, "kind": "done",
                    "bytes": tr.size, "total": tr.size, "path": path.clone() }));
                crate::finish_transfer(app, id, "recv", &tr.name, tr.size, None, None, Some(path), Ok(()));
                let mut seen = seen_read(app);
                seen["seen"][tr.tid.as_str()] = json!(now_ms());
                let mut receipts = seen["receipts"].as_array().cloned().unwrap_or_default();
                receipts.push(json!({ "device": peer, "tid": tr.tid }));
                seen["receipts"] = json!(receipts);
                seen_write(app, &seen);
                found += 1;
            }
            Err(e) => {
                crate::finish_transfer(app, id, "recv", &tr.name, tr.size, None, None, None, Err(e));
            }
        }
    }
    Ok(found)
}

/// Отправка подтверждений «получено» (повтор — раз в 10 минут до успеха).
pub(crate) async fn flush_receipts(app: &AppHandle) {
    let st = seen_read(app);
    let receipts = st["receipts"].as_array().cloned().unwrap_or_default();
    if receipts.is_empty() {
        return;
    }
    let mut remain = Vec::new();
    for r in receipts {
        let device = r["device"].as_str().unwrap_or("").to_string();
        let tid = r["tid"].as_str().unwrap_or("").to_string();
        if device.is_empty() || tid.is_empty() {
            continue;
        }
        let msg = format!("ndmx1:{}", B64.encode(json!({ "t": "got", "tid": tid }).to_string()));
        if crate::send_text_to_device(app.clone(), device, msg).await.is_err() {
            remain.push(r);
        }
    }
    let mut st = seen_read(app);
    st["receipts"] = json!(remain);
    seen_write(app, &st);
}

/// Ручная проверка почты (Mail.ru работает только здесь, с кредами).
#[tauri::command]
pub(crate) async fn mailbox_poll(
    app: AppHandle,
    mailru_token: Option<String>,
) -> Result<serde_json::Value, String> {
    let found = poll_inboxes(&app, mailru_token.as_deref()).await;
    Ok(json!({ "found": found }))
}

/// Автосоздание корневой папки почты при подключении облака: .netdrop/mailbox/
/// публикуется, ссылка владельца сохраняется в "_self" (для QR и автообмена).
#[tauri::command]
pub(crate) async fn mailbox_prepare(
    app: AppHandle,
    provider: String,
    token: String,
    account: String,
    login: String,
) -> Result<serde_json::Value, String> {
    let (url, gmbox) = match provider.as_str() {
        "yandex" => {
            ya_mkdir(&token, "disk:/.netdrop").await?;
            ya_mkdir(&token, "disk:/.netdrop/mailbox").await?;
            (ya_publish(&token, "disk:/.netdrop/mailbox").await?, String::new())
        }
        "gdrive" => {
            let root = match g_find_folder(&token, ".netdrop", None).await? {
                Some(id) => id,
                None => g_create_folder(&token, ".netdrop", None).await?,
            };
            let mb = match g_find_folder(&token, "mailbox", Some(&root)).await? {
                Some(id) => id,
                None => g_create_folder(&token, "mailbox", Some(&root)).await?,
            };
            g_anyone_reader(&token, &mb).await?;
            (String::new(), mb)
        }
        "mailru" => {
            let (l, p) = mr_split(&token)?;
            mr_mkcol(l, p, "/.netdrop").await?;
            mr_mkcol(l, p, "/.netdrop/mailbox").await?;
            ("/.netdrop/mailbox".to_string(), String::new())
        }
        other => return Err(format!("unknown provider '{other}'")),
    };
    let mut ob = outbox_read(&app);
    ob["_self"] = json!({
        "provider": provider, "account": account, "login": login,
        "url": url, "gmbox": gmbox,
    });
    outbox_write(&app, &ob);
    Ok(json!({ "url": ob["_self"]["url"] }))
}

/// Ссылка владельца на облачную почту (для QR сопряжения).
pub(crate) fn self_info(app: &AppHandle) -> Option<serde_json::Value> {
    outbox_read(app).get("_self").cloned()
}

/// Одноразовый ключ пары для QR сопряжения (живёт 15 минут).
pub(crate) fn qr_key_new(app: &AppHandle) -> Option<String> {
    use rand::RngCore;
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    let b = B64.encode(k);
    let mut ob = outbox_read(app);
    ob["_qrkey"] = json!({ "key": b, "ts": now_ms() });
    outbox_write(app, &ob);
    Some(b)
}

fn qr_key_take(app: &AppHandle) -> Option<String> {
    let mut ob = outbox_read(app);
    let k = ob.get("_qrkey")?.clone();
    let fresh = now_ms().saturating_sub(k["ts"].as_u64().unwrap_or(0)) < 15 * 60 * 1000;
    if let Some(o) = ob.as_object_mut() {
        o.remove("_qrkey");
    }
    outbox_write(app, &ob);
    if fresh {
        k["key"].as_str().map(String::from)
    } else {
        None
    }
}

/// Текст → временный файл (для отправки сообщений через облачный ящик).
#[tauri::command]
pub(crate) async fn mailbox_text_file(app: AppHandle, text: String) -> Result<String, String> {
    let cache = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&cache).await.ok();
    let p = cache.join(format!("netdrop-message-{}.txt", now_ms()));
    tokio::fs::write(&p, text.as_bytes()).await.map_err(|e| e.to_string())?;
    Ok(p.to_string_lossy().to_string())
}

/// Статус почты для UI: исходящие ящики, входящие, очередь подтверждений.
#[tauri::command(async)]
pub(crate) fn mailbox_status(app: AppHandle) -> serde_json::Value {
    let ob = outbox_read(&app);
    let mut outbox = Vec::new();
    if let Some(map) = ob.as_object() {
        for (dev, e) in map {
            if dev.starts_with('_') {
                continue; // служебные записи (_self, _qrkey)
            }
            let transfers = e["transfers"].as_array().cloned().unwrap_or_default();
            let pending = transfers
                .iter()
                .filter(|t| !t["confirmed"].as_bool().unwrap_or(false))
                .count();
            outbox.push(json!({
                "device": dev, "provider": e["provider"], "account": e["account"],
                "login": e["login"], "pending": pending,
            }));
        }
    }
    let inboxes: Vec<serde_json::Value> = crate::load_paired(&app)
        .into_iter()
        .filter(|d| !d["mbox"].is_null())
        .map(|d| json!({ "device": d["id"], "provider": d["mbox"]["provider"] }))
        .collect();
    let seen = seen_read(&app);
    json!({
        "outbox": outbox,
        "inboxes": inboxes,
        "receipts": seen["receipts"].as_array().map(|a| a.len()).unwrap_or(0),
        "pollMin": poll_minutes(),
    })
}

// ---------- Тесты ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_roundtrip() {
        let key = [3u8; 32];
        let idx = MboxIndex {
            v: 1,
            transfers: vec![MboxTransfer {
                tid: "aabbccdd00112233".into(),
                name: "фото.zip".into(),
                size: 42,
                chunks: 1,
                tar: false,
                key: B64.encode([7u8; 32]),
                ids: vec![],
                gfolder: String::new(),
                ts: 1,
                confirmed: false,
            }],
        };
        let enc = seal_index(&key, &idx).expect("seal");
        let dec = open_index(&key, &enc).expect("open");
        assert_eq!(dec.transfers.len(), 1);
        assert_eq!(dec.transfers[0].name, "фото.zip");
        // не тот ключ → отказ
        assert!(open_index(&[4u8; 32], &enc).is_err());
    }

    #[test]
    fn recip_is_16_chars() {
        assert_eq!(recip_of("0123456789abcdef0123456789abcdef").len(), 16);
    }
}
