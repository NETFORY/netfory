// ============================================================
//  SmartNet // messenger_launcher.rs (v1.135.217)
//
//  Запуск NETFORY Messenger (`ntfry-messenger[.exe]`) как дочернего процесса +
//  IPC по stdin/stdout (JSONL, типы — netfory_chat_core::ipc). Сид уходит по
//  stdin только в RAM двух процессов. Из stdout: непрочитанные (сайдбар/трей),
//  новые сообщения, запросы резолва имён, запрос платежа (этап 2 — отказ
//  «скоро»), «открыть в Netfory», запрос архива для миграции.
//  Поиск бинаря — как у oxid-mail: env NTFRY_MESSENGER_BIN → рядом с exe →
//  ./ntfry-messenger/ → PATH. Если бинаря нет — Netfory показывает встроенный чат.
// ============================================================
use netfory_chat_core::ipc::{ToHost, ToMessenger};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

#[cfg(windows)]
const EXE_NAME: &str = "ntfry-messenger.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "ntfry-messenger";

struct Proc {
    child: Child,
    stdin: ChildStdin,
}

static PROC: Mutex<Option<Proc>> = Mutex::new(None);

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("NTFRY_MESSENGER_BIN") {
        if !p.trim().is_empty() {
            out.push(PathBuf::from(p.trim()));
        }
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            out.push(dir.join(EXE_NAME));
            out.push(dir.join("ntfry-messenger").join(EXE_NAME));
        }
    }
    out
}

/// Бинарь мессенджера доступен (рядом с exe / env / PATH)?
#[tauri::command]
pub(crate) fn messenger_available() -> bool {
    candidates().iter().any(|c| c.is_file())
        || std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(EXE_NAME).is_file()))
            .unwrap_or(false)
}

#[tauri::command]
pub(crate) fn messenger_running() -> bool {
    PROC.lock().unwrap().is_some()
}

fn send(m: &ToMessenger) -> bool {
    let mut g = PROC.lock().unwrap();
    let Some(p) = g.as_mut() else { return false };
    let Ok(s) = serde_json::to_string(m) else { return false };
    writeln!(p.stdin, "{s}").is_ok() && p.stdin.flush().is_ok()
}

/// Архив чата для миграции: все sled-ключи чата (chat:/xkey:/group:/msg:/ban:/direct:/mcost:) → base64.
#[tauri::command]
pub(crate) fn chat_export_archive() -> Result<String, String> {
    use base64::Engine;
    let prefixes: [&[u8]; 8] = [b"chat:", b"chat_", b"xkey:", b"group:", b"msg:", b"ban:", b"direct:", b"mcost:"];
    let mut entries = Vec::new();
    for pfx in prefixes {
        for item in crate::DB.scan_prefix(pfx) {
            let Ok((k, v)) = item else { continue };
            let Ok(key) = String::from_utf8(k.to_vec()) else { continue };
            entries.push(serde_json::json!({ "k": key, "v": base64::engine::general_purpose::STANDARD.encode(&v) }));
        }
    }
    let out = serde_json::json!({ "v": 1, "from": "netfory", "entries": entries });
    serde_json::to_string(&out).map_err(|e| e.to_string())
}

fn to_json<T: serde::Serialize>(v: T) -> Result<serde_json::Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// Белый список команд Netfory, доступных мессенджеру по IPC (`HostCall`): общий UI чата
/// зовёт их как обычные Tauri-команды, мессенджер проксирует сюда. Аргументы — как из JS (camelCase).
async fn host_call(app: &tauri::AppHandle, cmd: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match cmd {
        "get_my_username" => to_json(crate::naming::get_my_username()),
        "recover_my_username" => to_json(crate::naming::recover_my_username().await),
        "username_available" => to_json(crate::naming::username_available(s("name"))?),
        "register_username" => to_json(crate::naming::register_username(s("name")).await?),
        "get_registration_fee" => to_json(crate::naming::get_registration_fee()),
        "username_of_address" => to_json(crate::naming::username_of_address(s("address"))),
        "resolve_username" => to_json(crate::naming::resolve_username(s("name"))),
        "get_active_smartnet_account" => to_json(crate::wallet::get_active_smartnet_account().await),
        "author_avatar" => to_json(crate::social::author_avatar(s("author")).await),
        "author_avatar_cached" => to_json(crate::social::author_avatar_cached(app.clone(), s("author")).await),
        "follow_stats" => to_json(crate::subs::follow_stats(app.clone(), s("address")).await),
        "net_online" => to_json(crate::netwatch::net_online()),
        "wallet_stream_status" => to_json(crate::wallet_stream::wallet_stream_status()),
        // Цепь для мессенджера: подпись — главным аккаунтом Netfory (адрес чата = адрес vault), сид не покидает Netfory.
        "sth_send_transfer" => {
            let secret = crate::SESSION_SEED.lock().unwrap().clone().ok_or("кошелёк заблокирован — войдите в Netfory")?;
            let (recipient, amount, fee, vendor) = (s("recipient"), s("amountSth"), s("feeSth"), s("vendor"));
            let out = tauri::async_runtime::spawn_blocking(move || crate::sth_node::send_transfer(&secret, &recipient, &amount, &fee, &vendor))
                .await
                .map_err(|e| e.to_string())??;
            to_json(out)
        }
        "sth_fetch_txs" => {
            let (p, v) = (s("param"), s("value"));
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as u32;
            let r = tauri::async_runtime::spawn_blocking(move || crate::sth_node::fetch_txs(&p, &v, limit)).await.map_err(|e| e.to_string())??;
            to_json(r)
        }
        "sth_fetch_history" => {
            let a = s("address");
            let limit = args.get("limit").and_then(|x| x.as_u64()).unwrap_or(100) as u32;
            let r = tauri::async_runtime::spawn_blocking(move || crate::sth_node::fetch_history(&a, limit)).await.map_err(|e| e.to_string())??;
            to_json(r)
        }
        "sth_transfer_dynamic_fee" => to_json(crate::sth_node::transfer_dynamic_fee(args.get("vendorLen").and_then(|x| x.as_u64()).unwrap_or(0) as usize)),
        "open_external" => {
            let allowed: Vec<String> = args
                .get("allowed")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            crate::clearnet::open_external(app.clone(), s("url"), allowed)?;
            Ok(serde_json::Value::Null)
        }
        _ => Err(format!("команда {cmd} недоступна из мессенджера")),
    }
}

fn handle_host_msg(app: &tauri::AppHandle, m: ToHost) {
    match m {
        ToHost::Ready { version, endpoint_id } => {
            crate::syslog::log("messenger", format!("ntfry-messenger {version} готов · endpoint {}…", &endpoint_id[..endpoint_id.len().min(10)]));
            let _ = app.emit("messenger-event", serde_json::json!({ "type": "ready", "version": version, "endpointId": endpoint_id }));
        }
        ToHost::Unread { total } => {
            crate::tray_set_unread(total);
            let _ = app.emit("messenger-event", serde_json::json!({ "type": "unread", "total": total }));
        }
        ToHost::NewMessage { room, from, preview } => {
            let _ = app.emit("messenger-event", serde_json::json!({ "type": "new_message", "room": room, "from": from, "preview": preview }));
        }
        ToHost::Resolve { req, kind, q } => {
            let value = if kind == "username" { crate::naming::resolve_username(q) } else { crate::naming::username_of_address(q) };
            send(&ToMessenger::Resolved { req, value });
        }
        ToHost::PayRequest { req, .. } => {
            // этап 2: подтверждение в кошельке Netfory
            send(&ToMessenger::PayResult { req, ok: false, tx_id: String::new(), error: "Платежи из мессенджера появятся в следующей версии".into() });
        }
        ToHost::ExportRequest { .. } => {
            if let Ok(json) = chat_export_archive() {
                send(&ToMessenger::Archive { json });
            }
        }
        ToHost::OpenInHost { url } => {
            if url == "netfory://messenger/export" {
                if let Ok(json) = chat_export_archive() {
                    send(&ToMessenger::Archive { json });
                }
                return;
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
            let _ = app.emit("netfory:open-address", url);
        }
        ToHost::Log { scope, msg } => crate::syslog::log(&format!("msg:{scope}"), msg),
        ToHost::HostCall { req, cmd, args } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let m = match host_call(&app, &cmd, args).await {
                    Ok(value) => ToMessenger::HostResult { req, ok: true, value, error: String::new() },
                    Err(error) => ToMessenger::HostResult { req, ok: false, value: serde_json::Value::Null, error },
                };
                send(&m);
            });
        }
    }
}

fn spawn_proc(app: &tauri::AppHandle, path: &std::ffi::OsStr) -> Result<(), String> {
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Не удалось запустить {}: {e}", path.to_string_lossy()))?;
    let mut stdin = child.stdin.take().ok_or("stdin pipe unavailable")?;
    let stdout = child.stdout.take().ok_or("stdout pipe unavailable")?;

    let seed = crate::SESSION_SEED.lock().unwrap().clone().ok_or("кошелёк заблокирован — войдите в Netfory")?;
    let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
    let lang = crate::DB.get(b"cfg:lang").ok().flatten().and_then(|v| String::from_utf8(v.to_vec()).ok()).unwrap_or_default();
    let hello = ToMessenger::Hello { seed, address, lang, data_dir: String::new() };
    writeln!(stdin, "{}", serde_json::to_string(&hello).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;

    let app2 = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(m) = serde_json::from_str::<ToHost>(&line) {
                handle_host_msg(&app2, m);
            }
        }
        if let Some(mut p) = PROC.lock().unwrap().take() {
            let _ = p.child.wait();
        }
        let _ = app2.emit("messenger-event", serde_json::json!({ "type": "exited" }));
    });
    *PROC.lock().unwrap() = Some(Proc { child, stdin });
    Ok(())
}

/// Запустить мессенджер (или сфокусировать окно). Возвращает путь бинаря.
#[tauri::command(async)]
pub(crate) fn launch_messenger(app: tauri::AppHandle) -> Result<String, String> {
    {
        let mut g = PROC.lock().unwrap();
        if let Some(p) = g.as_mut() {
            match p.child.try_wait() {
                Ok(None) => {
                    drop(g);
                    send(&ToMessenger::Focus);
                    return Ok("running".into());
                }
                _ => {
                    *g = None;
                }
            }
        }
    }
    for c in candidates() {
        if c.is_file() {
            spawn_proc(&app, c.as_os_str())?;
            return Ok(c.to_string_lossy().to_string());
        }
    }
    match spawn_proc(&app, std::ffi::OsStr::new(EXE_NAME)) {
        Ok(()) => Ok(EXE_NAME.to_string()),
        Err(_) => Err("ntfry-messenger не найден рядом с Netfory".into()),
    }
}

/// Открыть ссылку (u://, sth://contact, sn://group/…) в мессенджере; запускает его при необходимости.
#[tauri::command(async)]
pub(crate) fn messenger_open(app: tauri::AppHandle, url: String) -> Result<(), String> {
    launch_messenger(app)?;
    if !send(&ToMessenger::Open { url }) {
        return Err("мессенджер не отвечает".into());
    }
    Ok(())
}

/// Завершение сессии Netfory → закрыть мессенджер (как Oxid Mail).
pub(crate) fn shutdown() {
    send(&ToMessenger::Shutdown);
    if let Some(mut p) = PROC.lock().unwrap().take() {
        let _ = p.child.wait();
    }
}
