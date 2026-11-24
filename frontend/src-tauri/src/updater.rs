// ============================================================
//  SmartNet // updater.rs
//
//  Полностью децентрализованная P2P-система обновлений.
//    • Источник истины по версиям — блокчейн SmartHoldem (STH):
//      транзакции на адрес UPDATE_RECIPIENT от адреса-создателя
//      UPDATE_CREATOR, в поле vendorField (memo) строка
//      "<app>:<version>:<btih_hash>".
//    • Транспорт файлов — торрент-протокол (librqbit): magnet собирается
//      из btih-хэша, пакет качается в data/updates/v<version>/ и остаётся
//      на раздаче (клиент сидирует обновление другим узлам сети).
//    • Никаких центральных серверов и GitHub.
//
//  Author: SmartNet
// ============================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Адрес назначения (куда создатель шлёт анонсы обновлений).
pub(crate) const UPDATE_RECIPIENT: &str = "SaQUdQbxZGJdjzZpLPyDYAqtE47EupDaTe";
/// Доверенный адрес-отправитель (транзакции от прочих адресов игнорируются).
pub(crate) const UPDATE_CREATOR: &str = "SeZLuyhhYf2qxs4ArPJ71oEu3x8EsVw51C";
/// Имя приложения в memo.
pub(crate) const APP_NAME: &str = "smartnet";
/// Дефолтный период проверки (часы), если .env не задан.
const DEFAULT_CHECK_PERIOD_HOURS: u64 = 48;

/// Информация об обнаруженном обновлении.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateInfo {
    pub app: String,
    pub version: String,
    pub hash: String,
    pub magnet: String,
    /// info_hash активной раздачи (заполняется после старта загрузки).
    #[serde(rename = "infoHash", default)]
    pub info_hash: String,
}

/// manifest.json внутри торрент-раздачи релиза.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct UpdateManifest {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub releases: HashMap<String, String>,
    #[serde(default)]
    pub notes: String,
}

// ---------- Вспомогательные функции ----------

/// Период проверки обновлений (часы) из env `CHECK_UPDATE_PERIOD`, иначе 48.
fn check_period_hours() -> u64 {
    std::env::var("CHECK_UPDATE_PERIOD")
        .ok()
        .or_else(|| option_env!("CHECK_UPDATE_PERIOD").map(|s| s.to_string()))
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|h| *h > 0)
        .unwrap_or(DEFAULT_CHECK_PERIOD_HOURS)
}

/// Сравнение семантических версий: true, если `candidate` строго новее `current`.
/// Числовое покомпонентное сравнение (пре-релизные суффиксы игнорируются).
pub(crate) fn version_gt(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split(|c| c == '.' || c == '-' || c == '+')
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .map(|s| s.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let (a, b) = (parts(candidate), parts(current));
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Собрать magnet-ссылку из btih-хэша. dn = "<app_name>-<version>"
/// (напр. `smartnet-1.8.10`). Трекеры инжектируются торрент-движком.
/// ws= — BEP19 web-seed: HTTPS-зеркало папки версии с файлами релиза
/// ({WEB_SEED_UPDATE}<version>/ — любой торрент-клиент докачает по HTTP).
fn build_magnet(hash: &str, version: &str) -> String {
    let dn = format!("{}-{}", APP_NAME, version.trim());
    let ws = pct(&format!("{}{}/", WEB_SEED_UPDATE, version.trim()));
    format!(
        "magnet:?xt=urn:btih:{}&dn={}&ws={}",
        hash.trim().to_lowercase(),
        dn,
        ws
    )
}

/// Минимальный percent-encode для параметров magnet-ссылки.
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Корень папки обновлений: data/updates.
fn updates_root(app: &tauri::AppHandle) -> PathBuf {
    let root = crate::ensure_data_dir(app).join("updates");
    let _ = std::fs::create_dir_all(&root);
    root
}

/// Папка конкретной версии: data/updates/v<version>.
fn version_dir(app: &tauri::AppHandle, version: &str) -> PathBuf {
    updates_root(app).join(format!("v{}", version.trim().trim_start_matches('v')))
}

/// Рекурсивно найти первый файл с именем `filename` в дереве `root`.
fn find_file_recursive(root: &Path, filename: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().and_then(|n| n.to_str()) == Some(filename) {
            return Some(p);
        }
    }
    for d in dirs {
        if let Some(found) = find_file_recursive(&d, filename) {
            return Some(found);
        }
    }
    None
}

/// Рекурсивно найти первый файл с одним из расширений `exts` (без регистра).
fn find_by_ext(root: &Path, exts: &[&str]) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if let Some(ext) = p.extension().and_then(|x| x.to_str()) {
            if exts.iter().any(|w| w.eq_ignore_ascii_case(ext)) {
                return Some(p);
            }
        }
    }
    for d in dirs {
        if let Some(found) = find_by_ext(&d, exts) {
            return Some(found);
        }
    }
    None
}

/// Рекурсивно собрать имена всех файлов в дереве (для диагностики).
fn list_files_rec(root: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            list_files_rec(&p, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            out.push(name.to_string());
        }
    }
}

/// Ключи manifest.releases для данной ОС (в порядке приоритета).
fn os_manifest_keys(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["windows"],
        "macos" => &["macos"],
        "linux" => &["linux-deb", "linux-snap"],
        "android" => &["android"],
        _ => &[],
    }
}

/// Ожидаемые расширения инсталлятора для данной ОС (fallback-поиск).
fn ext_for_os(os: &str) -> &'static [&'static str] {
    match os {
        "windows" => &["msi", "exe"],
        "macos" => &["dmg"],
        "linux" => &["deb", "snap", "appimage"],
        "android" => &["apk"],
        _ => &[],
    }
}

/// Удалить из data/updates все папки версий, КРОМЕ перечисленных в `keep`.
/// `keep_newer_than`: не трогать пакеты версий НОВЕЕ указанной (скачанное, но
/// ещё не установленное обновление — после «Позже» + рестарт).
/// Перед удалением папки раздача пакета снимается из торрент-сессии: иначе
/// librqbit (persistence + fastresume) воскрешает её как «готовую», заново
/// создаёт 0-байтные файлы, и клиент получает «разбор manifest: expected value
/// at line 1 column 1» / «инсталлятор не найден» (v1.135.132 → .137, Win).
fn purge_updates(app: &tauri::AppHandle, keep: &[String], keep_newer_than: Option<&str>) {
    let keep_norm: Vec<String> = keep
        .iter()
        .map(|v| v.trim().trim_start_matches('v').to_string())
        .collect();
    let root = updates_root(app);
    let Ok(entries) = std::fs::read_dir(&root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with('v') {
            continue;
        }
        let ver = name.trim_start_matches('v').to_string();
        if keep_norm.contains(&ver) {
            continue;
        }
        if keep_newer_than.map(|cur| version_gt(&ver, cur)).unwrap_or(false) {
            continue;
        }
        let dropped = crate::torrent::forget_torrents_in_dir(&p);
        let _ = std::fs::remove_dir_all(&p);
        crate::syslog::log(
            "sys",
            format!("update cleanup: removed {} (torrents dropped: {dropped})", p.display()),
        );
    }
}

/// Стартовая зачистка: оставить папку текущей рабочей версии и пакеты более
/// новых версий (ожидающее установки обновление не скачиваем повторно).
/// Вызывается из `lib.rs` в `setup()`.
pub fn cleanup_on_start(app: &tauri::AppHandle) {
    let cur = env!("CARGO_PKG_VERSION");
    purge_updates(app, &[cur.to_string()], Some(cur));
}

/// Пакет на диске пригоден к установке: manifest.json читается и инсталлятор
/// для текущей ОС на месте (≥ 1 МБ). Используется, чтобы не доверять слепо
/// «уже сидируется» — раздача может быть зомби после удаления папки.
fn package_sane(dir: &Path) -> bool {
    let manifest_ok = find_file_recursive(dir, "manifest.json")
        .and_then(|p| std::fs::read(p).ok())
        .map(|raw| serde_json::from_slice::<UpdateManifest>(&normalize_json_bytes(&raw)).is_ok())
        .unwrap_or(false);
    manifest_ok && locate_installer(dir, std::env::consts::OS).is_ok()
}

// ---------- Tauri-команды ----------

/// Период проверки обновлений (часы) для фронтенд-таймера.
#[tauri::command]
pub fn get_update_period_hours() -> u64 {
    check_period_hours()
}

/// Текущая версия клиента (CARGO_PKG_VERSION).
#[tauri::command]
pub fn get_client_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// ОС для сопоставления с ключами manifest.releases.
/// windows | macos | linux | android
#[tauri::command]
pub fn current_os() -> String {
    std::env::consts::OS.to_string()
}

/// Просканировать блокчейн STH и вернуть обновление, если версия новее текущей.
/// Fallback: узлы блокчейна недоступны → читаем указатель update.enc с облаков
/// доверенных Cloud Seeder-ов (.env CLOUD_SEEDERS) — тот же формат JSON,
/// расшифровка локально, транспорт скачивания прежний (торрент по magnet).
#[tauri::command]
pub async fn check_for_update() -> Result<Option<UpdateInfo>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let chain = tauri::async_runtime::spawn_blocking(move || check_chain(current))
        .await
        .map_err(|e| e.to_string())?;
    match chain {
        Ok(best) => Ok(best),
        Err(chain_err) => {
            crate::syslog::log(
                "upd",
                "blockchain nodes unreachable — fallback: update.enc from trusted Cloud Seeders"
                    .to_string(),
            );
            match crate::cloudseeder::fetch_update_from_cloud().await {
                Some((version, hash)) if version_gt(&version, current) => {
                    crate::syslog::log(
                        "upd",
                        format!(
                            "cloud pointer: update v{version} available (btih {}…)",
                            hash.chars().take(12).collect::<String>()
                        ),
                    );
                    Ok(Some(UpdateInfo {
                        app: APP_NAME.to_string(),
                        magnet: build_magnet(&hash, &version),
                        version,
                        hash,
                        info_hash: String::new(),
                    }))
                }
                Some(_) => {
                    crate::syslog::log(
                        "upd",
                        format!("cloud pointer: no updates — v{current} is current"),
                    );
                    Ok(None)
                }
                None => Err(chain_err),
            }
        }
    }
}

/// Скан анонсов обновлений в блокчейне (блокирующий — вызывать из spawn_blocking).
fn check_chain(current: &str) -> Result<Option<UpdateInfo>, String> {
    crate::syslog::log(
        "upd",
        format!("checking for updates on STH blockchain (current version v{current})…"),
    );
    let txs = match crate::sth_node::fetch_txs("recipientId", UPDATE_RECIPIENT, 50) {
        Ok(t) => t,
        Err(e) => {
            crate::syslog::log(
                "upd",
                format!("failed to read update transactions: {e}"),
            );
            return Err(e);
        }
    };
    let mut best: Option<UpdateInfo> = None;
    for tx in txs {
        if tx.sender != UPDATE_CREATOR {
            continue;
        }
        // memo: "<app>:<version>:<btih_hash>"
        let parts: Vec<&str> = tx.vendor.trim().splitn(3, ':').collect();
        if parts.len() != 3 {
            continue;
        }
        let (app_name, version, hash) = (parts[0].trim(), parts[1].trim(), parts[2].trim());
        if app_name != APP_NAME || hash.is_empty() || version.is_empty() {
            continue;
        }
        if !version_gt(version, current) {
            continue;
        }
        let is_newer = best
            .as_ref()
            .map(|b| version_gt(version, &b.version))
            .unwrap_or(true);
        if is_newer {
            best = Some(UpdateInfo {
                app: app_name.to_string(),
                version: version.to_string(),
                hash: hash.to_string(),
                magnet: build_magnet(hash, version),
                info_hash: String::new(),
            });
        }
    }
    match &best {
        Some(b) => crate::syslog::log(
            "upd",
            format!(
                "update v{} available (btih {}…) — source resolved via torrent network",
                b.version,
                b.hash.chars().take(12).collect::<String>()
            ),
        ),
        None => crate::syslog::log(
            "upd",
            format!("no updates — v{current} is current"),
        ),
    }
    Ok(best)
}

/// Начать фоновую загрузку обновления в data/updates/v<version>.
/// Возвращает info_hash раздачи (для отслеживания прогресса на фронте).
/// Официальное HTTP-зеркало пакетов обновлений («web-seed»).
/// librqbit (9.0.0-rc) не поддерживает BEP19 url-list, поэтому web-seed
/// реализован прямой загрузкой: файлы пакета качаются по HTTPS с зеркала,
/// затем торрент верифицирует их и мгновенно переходит в Seeding.
const WEB_SEED_UPDATE: &str = "https://smartholdem.io/download/torrents/";

/// Скачать пакет версии с HTTP-зеркала в каталог пакета (best-effort).
/// Структура зеркала: {WEB_SEED_UPDATE}{version}/manifest.json + файлы релиза
/// + сам .torrent релиза (smartnet-<version>.torrent или release.torrent).
/// Возвращает байты .torrent, если зеркало его отдало: с ними метаданные НЕ
/// нужны из DHT — обновление работает даже при нуле живых сидов.
fn try_web_seed(version: &str, dir: &str) -> Option<Vec<u8>> {
    let base = format!("{}{}/", WEB_SEED_UPDATE, version);
    // (1) .torrent релиза — качаем в первую очередь (лёгкий, до 10 МБ).
    let mut torrent_bytes: Option<Vec<u8>> = None;
    for name in [format!("{}-{}.torrent", APP_NAME, version), "release.torrent".to_string()] {
        if let Ok(resp) = ureq::get(&format!("{}{}", base, name))
            .timeout(std::time::Duration::from_secs(30))
            .call()
        {
            let mut buf = Vec::new();
            use std::io::Read;
            if resp
                .into_reader()
                .take(10 * 1024 * 1024)
                .read_to_end(&mut buf)
                .is_ok()
                && !buf.is_empty()
                && buf.first() == Some(&b'd') // bencode-словарь, не HTML-страница ошибки
            {
                crate::syslog::log(
                    "upd",
                    format!("web-seed: fetched release .torrent ({name}, {} bytes)", buf.len()),
                );
                torrent_bytes = Some(buf);
                break;
            }
        }
    }
    // (2) Файлы пакета с зеркала (best-effort): торрент верифицирует готовые
    // файлы и мгновенно перейдёт в Seeding.
    let body = match ureq::get(&format!("{}manifest.json", base))
        .timeout(std::time::Duration::from_secs(15))
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
    {
        Some(b) => b,
        None => return torrent_bytes, // зеркало без manifest — обычный торрент-путь
    };
    let v: serde_json::Value = match serde_json::from_str(body.trim_start_matches('\u{feff}').trim_start()) {
        Ok(v) => v,
        Err(_) => return torrent_bytes,
    };
    if v["version"].as_str() != Some(version) {
        return torrent_bytes;
    }
    let _ = std::fs::create_dir_all(dir);
    let mut files: Vec<String> = Vec::new();
    if let Some(rel) = v["releases"].as_object() {
        files.extend(rel.values().filter_map(|x| x.as_str()).map(String::from));
    }
    if let Some(n) = v["notes"].as_str() {
        files.push(n.to_string());
    }
    let mut ok = 0usize;
    for f in &files {
        if f.is_empty() || f.contains("..") || f.contains('/') || f.contains('\\') {
            continue;
        }
        let dest = std::path::Path::new(dir).join(f);
        // готовый файл — только НЕпустой; 0-байтный огрызок (kill/рестарт во
        // время записи) удаляем и качаем заново
        if dest.is_file() {
            if std::fs::metadata(&dest).map(|m| m.len() > 0).unwrap_or(false) {
                ok += 1;
                continue;
            }
            let _ = std::fs::remove_file(&dest);
        }
        if let Ok(resp) = ureq::get(&format!("{}{}", base, f))
            .timeout(std::time::Duration::from_secs(600))
            .call()
        {
            let tmp = dest.with_extension("part");
            let saved = match std::fs::File::create(&tmp) {
                Ok(mut out) => match std::io::copy(&mut resp.into_reader(), &mut out) {
                    Ok(_) => true,
                    Err(e) => {
                        crate::crashdiag::note_io_error(&e, "update web-seed (write)");
                        false
                    }
                },
                Err(e) => {
                    crate::crashdiag::note_io_error(&e, "update web-seed (create)");
                    false
                }
            };
            if saved && std::fs::rename(&tmp, &dest).is_ok() {
                ok += 1;
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }
    if ok > 0 {
        // атомарно (.part → rename): fs::write при рестарте/kill оставлял
        // ПУСТОЙ manifest.json → «разбор manifest: expected value at line 1»
        let mpath = std::path::Path::new(dir).join("manifest.json");
        let mtmp = mpath.with_extension("json.part");
        if std::fs::write(&mtmp, body.as_bytes()).is_ok() {
            let _ = std::fs::rename(&mtmp, &mpath);
        }
        crate::syslog::log(
            "upd",
            format!("web-seed: fetched {ok}/{} package file(s) from {WEB_SEED_UPDATE}", files.len()),
        );
    }
    torrent_bytes
}

#[tauri::command]
pub async fn start_update_download(
    app: tauri::AppHandle,
    version: String,
    hash: String,
) -> Result<String, String> {
    let ih = hash.trim().to_uppercase();
    let vdir = version_dir(&app, &version);
    // Если релиз уже в раздаче (например, этот клиент сам его создал/сидирует) —
    // не качаем повторно «у самого себя», сразу возвращаем info_hash: фронтенд
    // увидит 100% и перейдёт к установке.
    if crate::torrent::is_managed(&ih) {
        // Раздача из папки updates/ обязана иметь целый пакет на диске. Иначе это
        // зомби: папку удалили (startup-cleanup старого клиента), а librqbit
        // с fastresume считает раздачу готовой и пересоздал пустые файлы.
        let from_updates = crate::torrent::folder_for(&ih)
            .map(|f| f.to_string_lossy().replace('\\', "/").contains("/updates/v"))
            .unwrap_or(false);
        if !from_updates || package_sane(&vdir) {
            crate::syslog::log(
                "upd",
                format!("v{version}: package already seeding locally — proceeding to install"),
            );
            return Ok(ih);
        }
        crate::syslog::log(
            "upd",
            format!(
                "v{version}: ZOMBIE seeding torrent — no valid package in {} (fastresume after folder purge); dropping and re-downloading",
                vdir.display()
            ),
        );
        let _ = crate::torrent::remove_torrent(ih.clone(), Some(false)).await;
        let _ = std::fs::remove_dir_all(&vdir);
    }
    let out = vdir.to_string_lossy().to_string();
    let magnet = build_magnet(&hash, &version);
    crate::syslog::log(
        "upd",
        format!("v{version}: starting torrent download of package (btih {}…) → {out}", ih.chars().take(12).collect::<String>()),
    );
    let app2 = app.clone();
    let ver2 = version.clone();
    let ih2 = ih.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // WEB-SEED: сперва пробуем стянуть пакет напрямую с HTTPS-зеркала —
        // торрент ниже верифицирует готовые файлы и мгновенно станет Seeding.
        // Если зеркало отдало и сам .torrent — добавляем раздачу ИЗ НЕГО:
        // метаданные не нужны из DHT, обновление идёт даже без живых сидов.
        let torrent_bytes = try_web_seed(&ver2, &out);
        let meta = match torrent_bytes {
            Some(tb) => match crate::torrent::add_update_from_bytes(&app, tb, &ih2, out.clone()) {
                Ok(m) => m,
                Err(e) => {
                    crate::syslog::log(
                        "upd",
                        format!("web-seed .torrent rejected ({e}) — falling back to magnet/DHT"),
                    );
                    crate::torrent::download_magnet_update(&app, magnet, out)?
                }
            },
            None => crate::torrent::download_magnet_update(&app, magnet, out)?,
        };
        // Фоновый логгер статуса обновления: помогает диагностировать, почему на
        // одних узлах пакет находится сразу, а на других «висит» (нет сидов/пиров,
        // не приходят метаданные из DHT).
        crate::torrent::log_update_progress(app2, meta.info_hash.clone(), ver2);
        Ok::<String, String>(meta.info_hash)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Прочитать manifest.json из скачанного пакета версии (рекурсивный поиск).
/// Поле `notes` может быть именем файла (напр. "CHANGELOG.md") — тогда его
/// содержимое подставляется как текст заметок к релизу.
/// serde_json не переваривает UTF-8 BOM и UTF-16 (типичны для manifest.json,
/// сохранённых Windows-редакторами: «Блокнот» пишет BOM/UTF-16 «Unicode»).
/// Нормализуем: UTF-16→UTF-8, отрезаем BOM и мусор до первого '{'.
fn normalize_json_bytes(raw: &[u8]) -> Vec<u8> {
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        let utf16: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if let Ok(s) = String::from_utf16(&utf16) {
            return normalize_json_bytes(s.as_bytes());
        }
    }
    if raw.len() >= 2 && raw[0] == 0xFE && raw[1] == 0xFF {
        let utf16: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        if let Ok(s) = String::from_utf16(&utf16) {
            return normalize_json_bytes(s.as_bytes());
        }
    }
    let mut s = raw;
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s = &s[3..];
    }
    match s.iter().position(|&b| b == b'{') {
        Some(pos) => s[pos..].to_vec(),
        None => s.to_vec(),
    }
}

#[tauri::command(async)]
pub fn read_update_manifest(
    app: tauri::AppHandle,
    version: String,
) -> Result<UpdateManifest, String> {
    let dir = version_dir(&app, &version);
    let path = find_file_recursive(&dir, "manifest.json")
        .ok_or_else(|| "manifest.json не найден в пакете обновления".to_string())?;
    let raw = std::fs::read(&path).map_err(|e| format!("чтение manifest: {e}"))?;
    if raw.iter().all(|b| b.is_ascii_whitespace() || *b == 0) {
        return Err("corrupt-package: manifest.json пустой (пакет повреждён — рестарт во время записи)".to_string());
    }
    let mut manifest: UpdateManifest = serde_json::from_slice(&normalize_json_bytes(&raw))
        .map_err(|e| format!("corrupt-package: разбор manifest: {e}"))?;
    // notes как имя файла → подгружаем его содержимое (обрезаем до 40 КБ).
    let notes_ref = manifest.notes.trim().to_string();
    if !notes_ref.is_empty() && !notes_ref.contains('\n') {
        if let Some(notes_path) = find_file_recursive(&dir, &notes_ref) {
            if let Ok(text) = std::fs::read_to_string(&notes_path) {
                manifest.notes = text.chars().take(40_000).collect();
            }
        }
    }
    Ok(manifest)
}

/// Сброс повреждённого пакета обновления: снимаем раздачу из сессии (с файлами)
/// и удаляем папку версии — следующий check скачает пакет заново с нуля.
/// Причина появления: рестарт/kill клиента во время записи файлов пакета →
/// 0-байтные файлы, а раздача в сессии помечена готовой (v1.135.129, Win10).
#[tauri::command]
pub async fn reset_update_package(
    app: tauri::AppHandle,
    version: String,
    hash: String,
) -> Result<(), String> {
    let ih = hash.trim().to_uppercase();
    if !ih.is_empty() {
        let _ = crate::torrent::remove_torrent(ih.clone(), Some(true)).await;
    }
    let dir = version_dir(&app, &version);
    let removed = std::fs::remove_dir_all(&dir).is_ok();
    crate::syslog::log(
        "upd",
        format!(
            "v{version}: package RESET (torrent {} dropped, dir removed: {removed}) — will re-download",
            ih.chars().take(12).collect::<String>()
        ),
    );
    Ok(())
}

/// Найти абсолютный путь к файлу инсталлятора для текущей ОС.
/// Порядок: (1) по имени из manifest.releases[<os-ключ>]; (2) fallback —
/// первый файл с подходящим расширением (.msi/.deb/.snap/.dmg/.apk).
/// В случае неудачи возвращает ошибку со списком файлов пакета (для диагностики).
#[tauri::command(async)]
pub fn resolve_installer(
    app: tauri::AppHandle,
    version: String,
    os_type: String,
) -> Result<String, String> {
    locate_installer(&version_dir(&app, &version), &os_type).map(|p| p.to_string_lossy().to_string())
}

/// Поиск инсталлятора в папке пакета (общая логика для команды и package_sane).
fn locate_installer(dir: &Path, os_type: &str) -> Result<PathBuf, String> {
    let os = os_type.to_lowercase();

    // Инсталлятор короче 1 МБ — огрызок после рестарта/kill во время записи:
    // не предлагаем к установке, сигналим фронтенду на сброс пакета.
    const MIN_INSTALLER_BYTES: u64 = 1024 * 1024;
    let sane = |p: &Path| std::fs::metadata(p).map(|m| m.len() >= MIN_INSTALLER_BYTES).unwrap_or(false);

    // (1) По имени из manifest.
    if let Some(mpath) = find_file_recursive(dir, "manifest.json") {
        if let Ok(raw) = std::fs::read(&mpath) {
            if let Ok(m) = serde_json::from_slice::<UpdateManifest>(&normalize_json_bytes(&raw)) {
                for key in os_manifest_keys(&os) {
                    if let Some(fname) = m.releases.get(*key) {
                        if let Some(p) = find_file_recursive(dir, fname.trim()) {
                            if !sane(&p) {
                                return Err(format!(
                                    "corrupt-package: инсталлятор {} усечён ({} байт)",
                                    fname.trim(),
                                    std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
                                ));
                            }
                            return Ok(p);
                        }
                    }
                }
            }
        }
    }

    // (2) Fallback: первый файл с подходящим расширением.
    let exts = ext_for_os(&os);
    if let Some(p) = find_by_ext(dir, exts) {
        if !sane(&p) {
            return Err(format!(
                "corrupt-package: инсталлятор {} усечён ({} байт)",
                p.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0)
            ));
        }
        return Ok(p);
    }

    // Диагностика: перечислим, что реально лежит в пакете.
    let mut files = Vec::new();
    list_files_rec(dir, &mut files);
    Err(format!(
        "инсталлятор для ОС '{os}' не найден (ожидались расширения: {:?}). Файлы в пакете: {:?}",
        exts, files
    ))
}

/// Устаревшая команда: поиск инсталлятора строго по имени файла.
#[tauri::command]
pub fn resolve_installer_path(
    app: tauri::AppHandle,
    version: String,
    filename: String,
) -> Result<String, String> {
    let dir = version_dir(&app, &version);
    find_file_recursive(&dir, &filename)
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| format!("файл инсталлятора не найден: {filename}"))
}

/// Удалить старые папки обновлений, оставив указанные версии.
#[tauri::command]
pub fn cleanup_update_dirs(app: tauri::AppHandle, keep_versions: Vec<String>) {
    purge_updates(&app, &keep_versions, None);
}

/// Определить ОС/формат по инсталлятору и запустить установку, затем закрыть
/// приложение. Windows → msiexec /i /qb; Linux → pkexec dpkg -i (deb) /
/// pkexec snap install --dangerous (snap); macOS → open (dmg); Android → JNI Intent (заглушка).
#[tauri::command(async)]
pub fn run_p2p_installer(installer_path: String, os_type: String) -> Result<(), String> {
    use std::process::Command;
    let p = Path::new(&installer_path);
    if !p.exists() {
        return Err(format!("инсталлятор не найден: {installer_path}"));
    }
    if std::fs::metadata(p).map(|m| m.len() < 1024 * 1024).unwrap_or(true) {
        return Err("corrupt-package: инсталлятор усечён — пакет будет скачан заново".to_string());
    }
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let os = os_type.to_lowercase();
    crate::syslog::log(
        "upd",
        format!(
            "launching installer ({}) — app will close and restart after install",
            p.file_name().and_then(|n| n.to_str()).unwrap_or("installer")
        ),
    );
    // Путь к текущему исполняемому файлу — для автоперезапуска после установки.
    let exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    match ext.as_str() {
        "msi" => {
            // Пишем временный .cmd-скрипт (пути на отдельных строках, без проблемы
            // вложенных кавычек cmd /C) и запускаем его отдельным процессом: скрипт
            // ждёт закрытия приложения → тихо ставит MSI → перезапускает клиент.
            let mut script = String::from("@echo off\r\n");
            script.push_str("timeout /t 2 /nobreak >nul\r\n");
            script.push_str(&format!("msiexec /i \"{installer_path}\" /qb\r\n"));
            if !exe.is_empty() {
                script.push_str(&format!("start \"\" \"{exe}\"\r\n"));
            }
            let bat = std::env::temp_dir().join("smartnet_update.cmd");
            std::fs::write(&bat, script).map_err(|e| format!("запись скрипта обновления: {e}"))?;
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&bat);
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                c.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            c.spawn().map_err(|e| format!("запуск скрипта обновления: {e}"))?;
            // штатный выход в обход event-loop — снимаем маркер грязного завершения
            let _ = std::fs::remove_file(crate::run_marker_path());
            std::process::exit(0);
        }
        "deb" | "snap" => {
            // pkexec запросит пароль, установит пакет и перезапустит клиент.
            let install = if ext == "deb" {
                format!("pkexec dpkg -i \"{installer_path}\"")
            } else {
                format!("pkexec snap install --dangerous \"{installer_path}\"")
            };
            let inner = if exe.is_empty() {
                install
            } else {
                format!("{install}; (\"{exe}\" &)")
            };
            Command::new("sh")
                .arg("-c")
                .arg(&inner)
                .spawn()
                .map_err(|e| format!("pkexec: {e}"))?;
            let _ = std::fs::remove_file(crate::run_marker_path());
            std::process::exit(0);
        }
        "dmg" => {
            // Монтируем .dmg; установка перетаскиванием — автоперезапуск не применим.
            Command::new("open")
                .arg(&installer_path)
                .spawn()
                .map_err(|e| format!("open: {e}"))?;
            let _ = std::fs::remove_file(crate::run_marker_path());
            std::process::exit(0);
        }
        "apk" => Err(
            "Android: установка выполняется системным Intent (JNI) — реализуется в мобильной обёртке."
                .to_string(),
        ),
        _ => Err(format!("неизвестный формат инсталлятора: .{ext} (os: {os})")),
    }
}
