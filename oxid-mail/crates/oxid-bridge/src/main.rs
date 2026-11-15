// oxid-bridge: HTTP-мост для web-режима Oxid Mail.
// Вся логика — в mail_core::service (общая с Tauri-версией), здесь только транспорт.
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tokio::task::spawn_blocking;
use tower_http::cors::CorsLayer;

use mail_core::identity::{self, Identity};
use mail_core::message::StoredMessage;
use mail_core::service::{self, AccountRequest, CoreError, MessagePatch, SendRequest};
use mail_store::MailStore;

struct Session {
    identity: Identity,
    store: Arc<MailStore>,
}

#[derive(Default)]
struct AppState {
    session: RwLock<Option<Session>>,
}

type St = State<Arc<AppState>>;
type ApiErr = (StatusCode, Json<Value>);

fn err_json(code: StatusCode, msg: impl Into<String>) -> ApiErr {
    (code, Json(json!({ "error": msg.into() })))
}

fn core_err(e: CoreError) -> ApiErr {
    match e {
        CoreError::NotFound(_) => err_json(StatusCode::NOT_FOUND, e.to_string()),
        CoreError::Msg(_) => err_json(StatusCode::BAD_REQUEST, e.to_string()),
    }
}

fn no_session() -> ApiErr {
    err_json(StatusCode::UNAUTHORIZED, "session not open")
}

async fn get_store(state: &Arc<AppState>) -> Result<Arc<MailStore>, ApiErr> {
    state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| s.store.clone())
        .ok_or_else(no_session)
}

async fn get_session(state: &Arc<AppState>) -> Result<(Arc<MailStore>, String), ApiErr> {
    state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| (s.store.clone(), s.identity.mnemonic.clone()))
        .ok_or_else(no_session)
}

/// spawn_blocking + маппинг ошибок ядра в HTTP-статусы.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, CoreError> + Send + 'static,
) -> Result<T, ApiErr> {
    spawn_blocking(f)
        .await
        .map_err(|e| err_json(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(core_err)
}

// ---------- identity / session ----------

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "service": "oxid-bridge", "version": env!("CARGO_PKG_VERSION") }))
}

async fn identity_generate() -> Json<Identity> {
    Json(identity::generate())
}

#[derive(serde::Deserialize)]
struct MnemonicReq {
    mnemonic: String,
}

async fn identity_import(Json(req): Json<MnemonicReq>) -> Result<Json<Identity>, ApiErr> {
    identity::from_mnemonic(&req.mnemonic)
        .map(Json)
        .map_err(|e| err_json(StatusCode::BAD_REQUEST, e))
}

async fn session_open(State(state): St, Json(req): Json<MnemonicReq>) -> Result<Json<Value>, ApiErr> {
    let probe = identity::from_secret(&req.mnemonic).map_err(|e| err_json(StatusCode::BAD_REQUEST, e))?;
    {
        let mut guard = state.session.write().await;
        match guard.as_ref() {
            // идемпотентность: тот же адрес — переиспользуем уже открытое хранилище
            Some(s) if s.identity.address == probe.address => {
                let store = s.store.clone();
                let publics = blocking(move || service::list_accounts(&store)).await?;
                return Ok(Json(json!({ "address": probe.address, "publicKey": probe.public_key, "accounts": publics })));
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
    let data_dir = std::env::var("OXID_DATA_DIR").unwrap_or_else(|_| "./oxid-data".into());
    let (ident, store, publics) =
        blocking(move || service::open_store(std::path::Path::new(&data_dir), &req.mnemonic)).await?;
    let resp = json!({ "address": ident.address, "publicKey": ident.public_key, "accounts": publics });
    let store = Arc::new(store);
    mail_core::idle::bump_generation();
    service::start_watchers(&store);
    mail_core::p2p::start(store.clone(), &ident.mnemonic);
    *state.session.write().await = Some(Session {
        identity: ident,
        store,
    });
    Ok(Json(resp))
}

async fn session_get(State(state): St) -> Json<Value> {
    match state.session.read().await.as_ref() {
        Some(s) => Json(json!({ "open": true, "address": s.identity.address, "publicKey": s.identity.public_key })),
        None => Json(json!({ "open": false })),
    }
}

async fn session_close(State(state): St) -> Json<Value> {
    if let Some(s) = state.session.write().await.take() {
        s.store.flush();
    }
    mail_core::idle::bump_generation();
    Json(json!({ "ok": true }))
}

// ---------- accounts ----------

async fn accounts_list(State(state): St) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let list = blocking(move || service::list_accounts(&store)).await?;
    Ok(Json(json!(list)))
}

async fn accounts_add(State(state): St, Json(req): Json<AccountRequest>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let acc = blocking(move || service::add_account(&store, req)).await?;
    Ok(Json(json!(acc)))
}

async fn accounts_update(
    State(state): St,
    Path(id): Path<String>,
    Json(req): Json<AccountRequest>,
) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let acc = blocking(move || service::update_account(&store, &id, req)).await?;
    Ok(Json(json!(acc)))
}

async fn accounts_delete(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    blocking(move || service::delete_account(&store, &id)).await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- sync / messages / send ----------

async fn account_sync(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let fetched = blocking(move || service::sync_account(&store, &id)).await?;
    Ok(Json(json!({ "fetched": fetched })))
}

async fn messages_list(State(state): St, Path(id): Path<String>) -> Result<Json<Vec<StoredMessage>>, ApiErr> {
    let store = get_store(&state).await?;
    let msgs = blocking(move || service::list_messages(&store, &id)).await?;
    Ok(Json(msgs))
}

async fn message_get(
    State(state): St,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<StoredMessage>, ApiErr> {
    let store = get_store(&state).await?;
    let msg = blocking(move || service::get_message(&store, &id, &mid)).await?;
    Ok(Json(msg))
}

async fn message_patch(
    State(state): St,
    Path((id, mid)): Path<(String, String)>,
    Json(patch): Json<MessagePatch>,
) -> Result<Json<StoredMessage>, ApiErr> {
    let store = get_store(&state).await?;
    let msg = blocking(move || service::patch_message(&store, &id, &mid, patch)).await?;
    Ok(Json(msg))
}

async fn message_delete(
    State(state): St,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    blocking(move || service::delete_message(&store, &id, &mid)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn account_send(
    State(state): St,
    Path(id): Path<String>,
    Json(req): Json<SendRequest>,
) -> Result<Json<StoredMessage>, ApiErr> {
    let (store, mnemonic) = get_session(&state).await?;
    let msg = blocking(move || service::send_message(&store, &mnemonic, &id, req)).await?;
    Ok(Json(msg))
}

// ---------- SmartHoldem: привязка / реестр / NTFRY ----------

async fn account_bind(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let (store, mnemonic) = get_session(&state).await?;
    let st = blocking(move || mail_core::crypto_mail::bind_email(&store, &mnemonic, &id)).await?;
    Ok(Json(json!(st)))
}

async fn account_bind_status(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let st = blocking(move || mail_core::crypto_mail::binding_status(&store, &id)).await?;
    Ok(Json(json!(st)))
}

async fn registry_sync(State(state): St) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let idx = blocking(move || mail_core::crypto_mail::registry_sync(&store)).await?;
    Ok(Json(json!({ "entries": idx.entries.len(), "seenCount": idx.seen_count, "syncedAt": idx.synced_at })))
}

#[derive(serde::Deserialize)]
struct LookupReq {
    email: String,
}

async fn registry_lookup(State(state): St, Json(req): Json<LookupReq>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let entry = blocking(move || mail_core::crypto_mail::lookup_email(&store, &req.email, false)).await?;
    Ok(Json(json!({ "entry": entry })))
}

async fn message_decrypt(
    State(state): St,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<Value>, ApiErr> {
    let (store, mnemonic) = get_session(&state).await?;
    let view = blocking(move || mail_core::crypto_mail::decrypt_message(&store, &mnemonic, &id, &mid)).await?;
    Ok(Json(json!(view)))
}

async fn message_verify(
    State(state): St,
    Path((id, mid)): Path<(String, String)>,
) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let res = blocking(move || mail_core::crypto_mail::verify_plain(&store, &id, &mid)).await?;
    Ok(Json(json!(res)))
}

// ---------- Контакты (Local Trust Layer) ----------

async fn contacts_list(State(state): St) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let list = blocking(move || mail_core::contacts::list(&store)).await?;
    Ok(Json(json!(list)))
}

#[derive(serde::Deserialize)]
struct ContactAddReq {
    /// Любая форма: email / sn://add-email?... / SADDR:email / SADDR@sth
    #[serde(default)]
    input: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    note: String,
}

async fn contacts_add(State(state): St, Json(req): Json<ContactAddReq>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let saved = blocking(move || {
        let mut c = mail_core::contacts::parse_any_resolved(&store, &req.input)?;
        if !req.name.is_empty() {
            c.name = req.name;
        }
        c.note = req.note;
        mail_core::contacts::add(&store, c)
    })
    .await?;
    Ok(Json(json!(saved)))
}

#[derive(serde::Deserialize)]
struct ContactParseReq {
    input: String,
}

async fn contacts_parse(State(state): St, Json(req): Json<ContactParseReq>) -> Result<Json<Value>, ApiErr> {
    // Без сессии — чистый разбор; с сессией — плюс резолвинг u://-имён
    let store = state.session.read().await.as_ref().map(|s| s.store.clone());
    let c = blocking(move || match &store {
        Some(st) => mail_core::contacts::parse_any_resolved(st, &req.input),
        None => mail_core::contacts::parse_any(&req.input),
    })
    .await?;
    Ok(Json(json!(c)))
}

/// Имя Naming Service (u://), зарегистрированное на адрес текущей сессии.
async fn naming_my(State(state): St) -> Result<Json<Value>, ApiErr> {
    let (store, addr) = state
        .session
        .read()
        .await
        .as_ref()
        .map(|s| (s.store.clone(), s.identity.address.clone()))
        .ok_or_else(no_session)?;
    let name = blocking(move || mail_core::naming::name_of(&store, &addr)).await?;
    Ok(Json(json!({ "name": name })))
}

async fn contacts_remove(State(state): St, Path(email): Path<String>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    blocking(move || mail_core::contacts::remove(&store, &email)).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn contact_card(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let (store, mnemonic) = get_session(&state).await?;
    let card = blocking(move || {
        let acc = mail_core::service::get_account(&store, &id)?;
        mail_core::contacts::my_card(&mnemonic, &acc.email, "")
    })
    .await?;
    Ok(Json(json!(card)))
}

// ---------- oauth ----------

/// Интерактивный вход Google (открывает браузер, блокирует до 180 сек).
async fn oauth_gmail(State(state): St) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let acc = blocking(move || service::oauth_gmail_sign_in(&store)).await?;
    Ok(Json(json!(acc)))
}

// ---------- pin / older / updates / attachments ----------

#[derive(serde::Deserialize)]
struct PinSealReq {
    mnemonic: String,
    pin: String,
}

async fn pin_seal(Json(req): Json<PinSealReq>) -> Result<Json<Value>, ApiErr> {
    let blob = service::pin_seal(&req.mnemonic, &req.pin).map_err(core_err)?;
    Ok(Json(json!({ "blob": blob })))
}

#[derive(serde::Deserialize)]
struct PinOpenReq {
    blob: String,
    pin: String,
}

async fn pin_open(Json(req): Json<PinOpenReq>) -> Result<Json<Value>, ApiErr> {
    let mnemonic = service::pin_open(&req.blob, &req.pin).map_err(core_err)?;
    Ok(Json(json!({ "mnemonic": mnemonic })))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OlderReq {
    folder: String,
    before_uid: u32,
    #[serde(default = "default_older_count")]
    count: u32,
}
fn default_older_count() -> u32 {
    50
}

async fn account_older(State(state): St, Path(id): Path<String>, Json(req): Json<OlderReq>) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let fetched = blocking(move || service::load_older(&store, &id, &req.folder, req.before_uid, req.count)).await?;
    Ok(Json(json!({ "fetched": fetched })))
}

async fn account_updates(State(state): St, Path(id): Path<String>) -> Result<Json<Value>, ApiErr> {
    let _ = get_store(&state).await?;
    Ok(Json(json!({ "dirty": mail_core::idle::take_dirty(&id) })))
}

/// Прогресс P2P-раздачи по progressId, который UI сгенерировал перед отправкой.
async fn p2p_progress(State(state): St, Path(pid): Path<String>) -> Result<Json<Value>, ApiErr> {
    let _ = get_store(&state).await?;
    Ok(Json(json!(mail_core::p2p::get_progress(&pid))))
}

async fn attachment_get(
    State(state): St,
    Path((id, mid, idx)): Path<(String, String, usize)>,
) -> Result<Json<Value>, ApiErr> {
    let store = get_store(&state).await?;
    let (meta, data) = blocking(move || service::get_attachment(&store, &id, &mid, idx)).await?;
    use base64::Engine as _;
    Ok(Json(json!({
        "name": meta.name,
        "mime": meta.mime,
        "size": meta.size,
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(&data)
    })))
}

#[tokio::main]
async fn main() {
    mail_core::init_tls();
    let state = Arc::new(AppState::default());
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/identity/generate", post(identity_generate))
        .route("/api/identity/import", post(identity_import))
        .route("/api/session/open", post(session_open))
        .route("/api/session", get(session_get))
        .route("/api/session/close", post(session_close))
        .route("/api/oauth/gmail", post(oauth_gmail))
        .route("/api/pin/seal", post(pin_seal))
        .route("/api/pin/open", post(pin_open))
        .route("/api/accounts", get(accounts_list).post(accounts_add))
        .route("/api/accounts/:id", put(accounts_update).delete(accounts_delete))
        .route("/api/accounts/:id/sync", post(account_sync))
        .route("/api/accounts/:id/older", post(account_older))
        .route("/api/accounts/:id/updates", get(account_updates))
        .route("/api/p2p/progress/:pid", get(p2p_progress))
        .route("/api/accounts/:id/messages", get(messages_list))
        .route(
            "/api/accounts/:id/messages/:mid",
            get(message_get).patch(message_patch).delete(message_delete),
        )
        .route("/api/accounts/:id/messages/:mid/attachments/:idx", get(attachment_get))
        .route("/api/accounts/:id/send", post(account_send))
        .route("/api/accounts/:id/bind", get(account_bind_status).post(account_bind))
        .route("/api/accounts/:id/messages/:mid/decrypt", post(message_decrypt))
        .route("/api/accounts/:id/messages/:mid/verify", get(message_verify))
        .route("/api/registry/sync", post(registry_sync))
        .route("/api/registry/lookup", post(registry_lookup))
        .route("/api/contacts", get(contacts_list).post(contacts_add))
        .route("/api/contacts/parse", post(contacts_parse))
        .route("/api/contacts/:email", axum::routing::delete(contacts_remove))
        .route("/api/naming/my", get(naming_my))
        .route("/api/accounts/:id/contact-card", get(contact_card))
        .layer(CorsLayer::very_permissive())
        .with_state(state);
    let port: u16 = std::env::var("OXID_BRIDGE_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8002);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind bridge port");
    println!("oxid-bridge listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await.expect("serve");
}
