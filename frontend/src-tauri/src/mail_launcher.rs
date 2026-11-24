// ============================================================
//  SmartNet // mail_launcher.rs
//
//  Запуск Oxid Mail как дочернего процесса + IPC-канал stdin/stdout (JSONL).
//  При запуске в stdin ребёнка уходит handshake с seed-фразой текущей
//  сессии SmartNet (seed живёт только в RAM двух процессов: не попадает
//  в argv/env/диск и не виден в диспетчере задач).
//  stdout ребёнка читает фоновый поток: события (new_mail и т.п.)
//  пересылаются в UI SmartNet событием "oxid-mail-event".
//  Повторный клик не плодит процессы — работающему окну шлётся focus.
//
//  Поиск бинаря:
//    1) env OXID_MAIL_BIN — явный путь;
//    2) рядом с текущим exe SmartNet (oxid-mail[.exe]) — сюда же Tauri
//       кладёт sidecar из bundle.externalBin (deb/AppImage: usr/bin,
//       macOS: Contents/MacOS);
//    3) подпапка ./oxid-mail/ рядом с exe;
//    4) tauri resource dir (Linux: usr/lib/<app>/ — старые сборки
//       с bundle.resources);
//    5) PATH.
// ============================================================
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;

use tauri::Emitter;

#[cfg(windows)]
const EXE_NAME: &str = "oxid-mail.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "oxid-mail";

struct MailProc {
    child: Child,
    stdin: ChildStdin,
}

static MAIL_PROC: Mutex<Option<MailProc>> = Mutex::new(None);

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("OXID_MAIL_BIN") {
        if !p.trim().is_empty() {
            out.push(PathBuf::from(p.trim()));
        }
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            out.push(dir.join(EXE_NAME));
            out.push(dir.join("oxid-mail").join(EXE_NAME));
        }
    }
    out
}

fn send_line(stdin: &mut ChildStdin, v: &serde_json::Value) -> std::io::Result<()> {
    writeln!(stdin, "{v}")?;
    stdin.flush()
}

fn spawn_mail(app: &tauri::AppHandle, path: &std::ffi::OsStr) -> Result<(), String> {
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Не удалось запустить {}: {e}", path.to_string_lossy()))?;
    let mut stdin = child.stdin.take().ok_or("stdin pipe unavailable")?;
    let stdout = child.stdout.take().ok_or("stdout pipe unavailable")?;

    // Handshake: seed текущей сессии (если кошелёк SmartNet разблокирован).
    // Без разблокировки почта запустится в автономном режиме (свой онбординг).
    if let Some(seed) = crate::SESSION_SEED.lock().unwrap().clone() {
        let _ = send_line(
            &mut stdin,
            &serde_json::json!({ "type": "handshake", "v": 1, "mnemonic": seed }),
        );
    }

    // Фоновый читатель событий почты → UI SmartNet
    let app = app.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                // pull-модель: почта просит seed (авто-логин / кнопка Connect SmartNet)
                if v.get("type").and_then(|t| t.as_str()) == Some("request_seed") {
                    if let Some(seed) = crate::SESSION_SEED.lock().unwrap().clone() {
                        if let Some(p) = MAIL_PROC.lock().unwrap().as_mut() {
                            let _ = send_line(
                                &mut p.stdin,
                                &serde_json::json!({ "type": "handshake", "v": 1, "mnemonic": seed }),
                            );
                        }
                    }
                    continue;
                }
                let _ = app.emit("oxid-mail-event", v);
            }
        }
        // Ребёнок завершился — освобождаем слот и собираем зомби-процесс
        if let Some(mut p) = MAIL_PROC.lock().unwrap().take() {
            let _ = p.child.wait();
        }
    });

    *MAIL_PROC.lock().unwrap() = Some(MailProc { child, stdin });
    Ok(())
}

/// Запускает Oxid Mail (или фокусирует уже открытое окно); возвращает путь бинаря.
#[tauri::command(async)]
pub fn launch_oxid_mail(app: tauri::AppHandle) -> Result<String, String> {
    {
        let mut guard = MAIL_PROC.lock().unwrap();
        if let Some(p) = guard.as_mut() {
            let running = matches!(p.child.try_wait(), Ok(None));
            if running && send_line(&mut p.stdin, &serde_json::json!({ "type": "focus" })).is_ok() {
                return Ok("already-running".into());
            }
            guard.take();
        }
    }
    let mut cands = candidates();
    // Ресурсная папка Tauri (Linux deb/AppImage: usr/lib/<app>/) — там
    // оказывается бинарь у сборок, упаковывавших его через bundle.resources.
    if let Ok(rd) = tauri::Manager::path(&app).resource_dir() {
        cands.push(rd.join(EXE_NAME));
    }
    for c in cands {
        if c.is_file() {
            // resources в бандле могут потерять бит исполнения — чиним, если можем.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(&c) {
                    if meta.permissions().mode() & 0o111 == 0 {
                        let _ = std::fs::set_permissions(&c, std::fs::Permissions::from_mode(0o755));
                    }
                }
            }
            spawn_mail(&app, c.as_os_str())?;
            return Ok(c.display().to_string());
        }
    }
    // последний шанс — системный PATH
    match spawn_mail(&app, std::ffi::OsStr::new(EXE_NAME)) {
        Ok(()) => Ok(EXE_NAME.to_string()),
        Err(_) => Err(
            "Oxid Mail не найден. Положите бинарь oxid-mail рядом с SmartNet, \
             в подпапку ./oxid-mail/ или укажите путь в переменной OXID_MAIL_BIN."
                .to_string(),
        ),
    }
}
