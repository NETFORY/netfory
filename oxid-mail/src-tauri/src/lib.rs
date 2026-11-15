// Oxid Mail — Tauri-бэкенд. Команды 1-в-1 повторяют oxid-bridge:
// вся логика в mail_core::service, здесь только транспорт + состояние сессии.
// Правило R1: тяжёлые операции (IMAP/SMTP/диск) — только в spawn_blocking.
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::async_runtime::spawn_blocking;
use tauri::Manager;
use tokio::sync::RwLock;

use mail_core::identity::{self, Identity};
use mail_core::message::StoredMessage;
use mail_core::service::{self, AccountRequest, MessagePatch, SendRequest};
use mail_store::MailStore;

struct Session {
    identity: Identity,
    store: Arc<MailStore>,
}

struct AppState {
    session: RwLock<Option<Session>>,
    data_dir: PathBuf,
}

type St<'a> = tauri::State<'a, AppState>;

async fn store_of(state: &St<'_>) -> Result<Arc<MailStore>, String> {
    state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| s.store.clone())
        .ok_or_else(|| "session not open".to_string())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, service::CoreError> + Send + 'static,
) -> Result<T, String> {
    spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

// ---------- SmartNet IPC (stdin/stdout, JSONL) ----------
// SmartNet запускает Oxid Mail дочерним процессом и передаёт seed через
// stdin-handshake (seed живёт только в RAM, на диск не пишется — PIN не нужен).
// stdout — канал событий обратно в SmartNet (new_mail и др.).

static HANDOFF_SEED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

fn ipc_send(v: Value) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn start_stdin_ipc() {
    std::thread::spawn(|| {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("handshake") => {
                    if let Some(m) = v.get("mnemonic").and_then(|m| m.as_str()) {
                        *HANDOFF_SEED.lock().unwrap() = Some(m.to_string());
                        ipc_send(json!({ "type": "handshake_ack" }));
                    }
                }
                Some("focus") => {
                    if let Some(app) = APP_HANDLE.get() {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

/// UI забирает seed, переданный SmartNet при запуске (одноразово).
#[tauri::command(async)]
fn handoff_take() -> Option<String> {
    HANDOFF_SEED.lock().unwrap().take()
}

/// Запрос seed у SmartNet по IPC (pull-модель: авто-логин и кнопка Connect SmartNet).
#[tauri::command(async)]
fn request_smartnet_seed() {
    ipc_send(json!({ "type": "request_seed" }));
}

/// Счётчик непрочитанных → SmartNet по IPC (бейдж на карточке Mail).
#[tauri::command(async)]
fn report_unread(count: u32) {
    ipc_send(json!({ "type": "unread", "count": count }));
}

/// Системное уведомление о новых письмах + событие в SmartNet по IPC.
#[tauri::command(async)]
fn notify_new_mail(app: tauri::AppHandle, count: u32, from: String, subject: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    let body = if count > 1 {
        format!("Новых писем: {count}")
    } else {
        format!("{from} — {subject}")
    };
    app.notification()
        .builder()
        .title("Oxid Mail")
        .body(&body)
        .show()
        .map_err(|e| e.to_string())?;
    ipc_send(json!({ "type": "new_mail", "count": count, "from": from, "subject": subject }));
    Ok(())
}

// ---------- identity / session ----------

#[tauri::command(async)]
fn identity_generate() -> Identity {
    identity::generate()
}

#[tauri::command(async)]
fn identity_import(mnemonic: String) -> Result<Identity, String> {
    identity::from_mnemonic(&mnemonic)
}

/// Импорт произвольной фразы SmartNet (без BIP-39 валидации) — для handoff.
#[tauri::command(async)]
fn identity_import_secret(secret: String) -> Result<Identity, String> {
    identity::from_secret(&secret)
}

#[tauri::command(async)]
async fn session_open(state: St<'_>, mnemonic: String) -> Result<Value, String> {
    let probe = mail_core::identity::from_secret(&mnemonic)?;
    {
        let mut guard = state.session.write().await;
        match guard.as_ref() {
            // идемпотентность: тот же адрес — переиспользуем уже открытое хранилище
            Some(s) if s.identity.address == probe.address => {
                let store = s.store.clone();
                let publics = blocking(move || service::list_accounts(&store)).await?;
                return Ok(json!({ "address": probe.address, "publicKey": probe.public_key, "accounts": publics }));
            }
            // другой адрес — закрываем старую сессию перед открытием новой
            Some(_) => {
                if let Some(old) = guard.take() {
                    old.store.flush();
                }
                mail_core::idle::bump_generation();
            }
            None => {}
        }
    }
    let dir = state.data_dir.clone();
    let (ident, store, publics) = spawn_blocking(move || service::open_store(&dir, &mnemonic))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let resp = json!({ "address": ident.address, "publicKey": ident.public_key, "accounts": publics });
    let store = Arc::new(store);
    mail_core::idle::bump_generation();
    service::start_watchers(&store);
    mail_core::p2p::start(store.clone(), &ident.mnemonic);
    *state.session.write().await = Some(Session {
        identity: ident,
        store,
    });
    Ok(resp)
}

#[tauri::command(async)]
async fn session_get(state: St<'_>) -> Result<Value, String> {
    Ok(match state.session.read().await.as_ref() {
        Some(s) => json!({ "open": true, "address": s.identity.address, "publicKey": s.identity.public_key }),
        None => json!({ "open": false }),
    })
}

#[tauri::command(async)]
async fn session_close(state: St<'_>) -> Result<Value, String> {
    if let Some(s) = state.session.write().await.take() {
        s.store.flush();
    }
    Ok(json!({ "ok": true }))
}

// ---------- accounts ----------

#[tauri::command(async)]
async fn accounts_list(state: St<'_>) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let list = blocking(move || service::list_accounts(&store)).await?;
    Ok(json!(list))
}

#[tauri::command(async)]
async fn account_add(state: St<'_>, req: AccountRequest) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let acc = blocking(move || service::add_account(&store, req)).await?;
    Ok(json!(acc))
}

#[tauri::command(async)]
async fn account_update(state: St<'_>, id: String, req: AccountRequest) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let acc = blocking(move || service::update_account(&store, &id, req)).await?;
    Ok(json!(acc))
}

#[tauri::command(async)]
async fn account_delete(state: St<'_>, id: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    blocking(move || service::delete_account(&store, &id)).await?;
    Ok(json!({ "ok": true }))
}

// ---------- sync / messages / send ----------

#[tauri::command(async)]
async fn account_sync(state: St<'_>, id: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let fetched = blocking(move || service::sync_account(&store, &id)).await?;
    Ok(json!({ "fetched": fetched }))
}

#[tauri::command(async)]
async fn messages_list(state: St<'_>, id: String) -> Result<Vec<StoredMessage>, String> {
    let store = store_of(&state).await?;
    blocking(move || service::list_messages(&store, &id)).await
}

#[tauri::command(async)]
async fn message_get(state: St<'_>, id: String, mid: String) -> Result<StoredMessage, String> {
    let store = store_of(&state).await?;
    blocking(move || service::get_message(&store, &id, &mid)).await
}

#[tauri::command(async)]
async fn message_patch(
    state: St<'_>,
    id: String,
    mid: String,
    patch: MessagePatch,
) -> Result<StoredMessage, String> {
    let store = store_of(&state).await?;
    blocking(move || service::patch_message(&store, &id, &mid, patch)).await
}

#[tauri::command(async)]
async fn message_delete(state: St<'_>, id: String, mid: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    blocking(move || service::delete_message(&store, &id, &mid)).await?;
    Ok(json!({ "ok": true }))
}

async fn session_of(state: &St<'_>) -> Result<(Arc<MailStore>, String), String> {
    state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| (s.store.clone(), s.identity.mnemonic.clone()))
        .ok_or_else(|| "session not open".to_string())
}

#[tauri::command(async)]
async fn account_send(state: St<'_>, id: String, req: SendRequest) -> Result<StoredMessage, String> {
    let (store, mnemonic) = session_of(&state).await?;
    blocking(move || service::send_message(&store, &mnemonic, &id, req)).await
}

#[tauri::command(async)]
async fn account_bind(state: St<'_>, id: String) -> Result<Value, String> {
    let (store, mnemonic) = session_of(&state).await?;
    let st = blocking(move || mail_core::crypto_mail::bind_email(&store, &mnemonic, &id)).await?;
    Ok(json!(st))
}

#[tauri::command(async)]
async fn account_bind_status(state: St<'_>, id: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let st = blocking(move || mail_core::crypto_mail::binding_status(&store, &id)).await?;
    Ok(json!(st))
}

#[tauri::command(async)]
async fn registry_sync(state: St<'_>) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let idx = blocking(move || mail_core::crypto_mail::registry_sync(&store)).await?;
    Ok(json!({ "entries": idx.entries.len(), "seenCount": idx.seen_count, "syncedAt": idx.synced_at }))
}

#[tauri::command(async)]
async fn registry_lookup(state: St<'_>, email: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let entry = blocking(move || mail_core::crypto_mail::lookup_email(&store, &email, false)).await?;
    Ok(json!({ "entry": entry }))
}

#[tauri::command(async)]
async fn message_decrypt(state: St<'_>, id: String, mid: String) -> Result<Value, String> {
    let (store, mnemonic) = session_of(&state).await?;
    let view = blocking(move || mail_core::crypto_mail::decrypt_message(&store, &mnemonic, &id, &mid)).await?;
    Ok(json!(view))
}

#[tauri::command(async)]
async fn message_verify(state: St<'_>, id: String, mid: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let res = blocking(move || mail_core::crypto_mail::verify_plain(&store, &id, &mid)).await?;
    Ok(json!(res))
}

#[tauri::command(async)]
async fn contacts_list(state: St<'_>) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let list = blocking(move || mail_core::contacts::list(&store)).await?;
    Ok(json!(list))
}

#[tauri::command(async)]
async fn contact_add(state: St<'_>, input: String, name: String, note: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let saved = blocking(move || {
        let mut c = mail_core::contacts::parse_any_resolved(&store, &input)?;
        if !name.is_empty() {
            c.name = name;
        }
        c.note = note;
        mail_core::contacts::add(&store, c)
    })
    .await?;
    Ok(json!(saved))
}

#[tauri::command(async)]
async fn contact_parse(state: St<'_>, input: String) -> Result<Value, String> {
    // Без сессии — чистый разбор; с сессией — плюс резолвинг u://-имён
    let store = state.session.read().await.as_ref().map(|s| s.store.clone());
    let c = blocking(move || match &store {
        Some(st) => mail_core::contacts::parse_any_resolved(st, &input),
        None => mail_core::contacts::parse_any(&input),
    })
    .await?;
    Ok(json!(c))
}

/// Имя Naming Service (u://), зарегистрированное на адрес текущей сессии.
#[tauri::command(async)]
async fn naming_my(state: St<'_>) -> Result<Value, String> {
    let (store, addr) = state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| (s.store.clone(), s.identity.address.clone()))
        .ok_or_else(|| "session not open".to_string())?;
    let name = blocking(move || mail_core::naming::name_of(&store, &addr)).await?;
    Ok(json!({ "name": name }))
}

#[tauri::command(async)]
async fn contact_remove(state: St<'_>, email: String) -> Result<Value, String> {
    let store = store_of(&state).await?;
    blocking(move || mail_core::contacts::remove(&store, &email)).await?;
    Ok(json!({ "ok": true }))
}

#[tauri::command(async)]
async fn contact_card(state: St<'_>, id: String) -> Result<Value, String> {
    let (store, mnemonic) = session_of(&state).await?;
    let card = blocking(move || {
        let acc = mail_core::service::get_account(&store, &id)?;
        mail_core::contacts::my_card(&mnemonic, &acc.email, "")
    })
    .await?;
    Ok(json!(card))
}

/// Открытие http(s)-ссылок из писем в системном браузере по умолчанию.
#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    let u = url.trim();
    if !(u.starts_with("http://") || u.starts_with("https://")) {
        return Err("разрешены только http(s)-ссылки".into());
    }
    open::that_detached(u).map_err(|e| e.to_string())
}

#[tauri::command(async)]
async fn oauth_gmail_sign_in(state: St<'_>) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let acc = blocking(move || service::oauth_gmail_sign_in(&store)).await?;
    Ok(json!(acc))
}

#[tauri::command(async)]
fn pin_seal(mnemonic: String, pin: String) -> Result<Value, String> {
    let blob = service::pin_seal(&mnemonic, &pin).map_err(|e| e.to_string())?;
    Ok(json!({ "blob": blob }))
}

#[tauri::command(async)]
fn pin_open(blob: String, pin: String) -> Result<Value, String> {
    let mnemonic = service::pin_open(&blob, &pin).map_err(|e| e.to_string())?;
    Ok(json!({ "mnemonic": mnemonic }))
}

#[tauri::command(async)]
async fn account_older(state: St<'_>, id: String, folder: String, before_uid: u32, count: Option<u32>) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let fetched = blocking(move || service::load_older(&store, &id, &folder, before_uid, count.unwrap_or(50))).await?;
    Ok(json!({ "fetched": fetched }))
}

#[tauri::command(async)]
async fn account_updates(state: St<'_>, id: String) -> Result<Value, String> {
    let _ = store_of(&state).await?;
    Ok(json!({ "dirty": mail_core::idle::take_dirty(&id) }))
}

/// Прогресс P2P-раздачи по progressId, сгенерированному UI перед отправкой.
#[tauri::command(async)]
async fn p2p_progress(state: St<'_>, pid: String) -> Result<Value, String> {
    let _ = store_of(&state).await?;
    Ok(json!(mail_core::p2p::get_progress(&pid)))
}

#[tauri::command(async)]
async fn attachment_get(state: St<'_>, id: String, mid: String, idx: usize) -> Result<Value, String> {
    let store = store_of(&state).await?;
    let (meta, data) = blocking(move || service::get_attachment(&store, &id, &mid, idx)).await?;
    use base64::Engine as _;
    Ok(json!({
        "name": meta.name,
        "mime": meta.mime,
        "size": meta.size,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(&data)
    }))
}

/// Экспорт seed-фразы: системный диалог «Сохранить как…» → запись JSON.
/// Ok(true) — сохранено, Ok(false) — пользователь отменил диалог.
#[tauri::command(async)]
async fn export_seed_file(
    app: tauri::AppHandle,
    file_name: String,
    contents: String,
) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_file_name(&file_name)
            .add_filter("JSON", &["json"])
            .blocking_save_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(fp) = picked else { return Ok(false) };
    let path = fp.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path, contents).map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn run() {
    mail_core::init_tls();
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // OXID_DATA_DIR переопределяет каталог данных (dev); по умолчанию — appData ОС
            let data_dir = std::env::var("OXID_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| app.path().app_data_dir().expect("app data dir"));
            std::fs::create_dir_all(&data_dir).ok();
            app.manage(AppState {
                session: RwLock::new(None),
                data_dir,
            });
            APP_HANDLE.set(app.handle().clone()).ok();
            start_stdin_ipc();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            identity_generate,
            identity_import,
            identity_import_secret,
            session_open,
            session_get,
            session_close,
            oauth_gmail_sign_in,
            handoff_take,
            request_smartnet_seed,
            notify_new_mail,
            report_unread,
            pin_seal,
            pin_open,
            account_older,
            account_updates,
            p2p_progress,
            attachment_get,
            accounts_list,
            account_add,
            account_update,
            account_delete,
            account_sync,
            messages_list,
            message_get,
            message_patch,
            message_delete,
            account_send,
            account_bind,
            account_bind_status,
            registry_sync,
            registry_lookup,
            message_decrypt,
            message_verify,
            contacts_list,
            contact_add,
            contact_parse,
            contact_remove,
            contact_card,
            open_external,
            export_seed_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Oxid Mail");
}
