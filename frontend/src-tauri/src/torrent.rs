/*
 * SmartNet // Встроенный BitTorrent-клиент (Этап A — Rust-ядро)
 *
 *   Модуль-обёртка над `librqbit` 8.1.1 (чистый Rust на Tokio, встроенный DHT,
 *   дисковый IO под большие файлы). Здесь живёт глобальный потокобезопасный
 *   TorrentManager, парсинг magnet-ссылок, запуск/пауза/удаление загрузок и
 *   отдача состояния (snapshot + событие `torrent-progress`) во Vue-фронтенд.
 *
 *   Гейтится фичей `p2p` (десктоп-сборка, где доступен tokio-рантайм). В
 *   web/не-p2p сборке команды возвращают понятную ошибку.
 *
 *   Метаданные торрентов хранятся в ОТДЕЛЬНОМ пространстве имён Sled —
 *   дерево "torrents" (см. `meta_tree`), что изолирует их от vault/dApp-индекса
 *   и позволяет масштабироваться на тысячи раздач.
 */

use serde::{Deserialize, Serialize};

/// Один файл внутри торрента (для дерева файлов в модалке).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentFile {
    pub path: String,
    pub size: u64,
}

/// Метаданные торрента, подгруженные из сети (ответ `parse_magnet_link`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentMetadata {
    #[serde(rename = "infoHash")]
    pub info_hash: String,
    pub name: String,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    pub files: Vec<TorrentFile>,
}

/// Живой статус активной/завершённой раздачи (для страницы Torrents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentStatus {
    #[serde(rename = "infoHash")]
    pub info_hash: String,
    pub name: String,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    #[serde(rename = "downloadedBytes")]
    pub downloaded_bytes: u64,
    #[serde(rename = "uploadedBytes")]
    pub uploaded_bytes: u64,
    pub progress: f32,
    /// Downloading | Seeding | Paused | Checking | Error
    pub state: String,
    #[serde(rename = "seedersConnected")]
    pub seeders_connected: u32,
    #[serde(rename = "peersConnected")]
    pub peers_connected: u32,
    #[serde(rename = "downloadSpeedBytes")]
    pub download_speed_bytes: u64,
    #[serde(rename = "uploadSpeedBytes")]
    pub upload_speed_bytes: u64,
    /// Число активных пир-соединений по TCP / µTP (для индикатора типа связи).
    #[serde(rename = "liveTcp")]
    pub live_tcp: u32,
    #[serde(rename = "liveUtp")]
    pub live_utp: u32,
    /// true, если это торрент пакета обновления (раздаётся из data/updates).
    #[serde(rename = "isUpdate")]
    pub is_update: bool,
}

/// Запись метаданных в Sled-дереве "torrents" (для resume после перезапуска).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TorrentRecord {
    info_hash: String,
    name: String,
    total_bytes: u64,
    magnet: String,
    save_path: Option<String>,
    /// Понятный абсолютный путь до персональной папки торрента в downloads
    /// (base/<name>). Дублирует save_path, но хранится явно для UI/отладки.
    #[serde(default)]
    folder: String,
    /// Состояние «на паузе» — восстанавливается 1:1 после перезапуска.
    #[serde(default)]
    paused: bool,
    sequential: bool,
    added_at: String,
}

// ============================================================
//  Реализация поверх librqbit (только в desktop/p2p-сборке)
// ============================================================
#[cfg(feature = "p2p")]
mod imp {
    use super::*;
    use librqbit::{
        AddTorrent, AddTorrentOptions, AddTorrentResponse, ListenerMode,
        ListenerOptions, Session, SessionOptions, SessionPersistenceConfig,
    };
    use once_cell::sync::Lazy;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::num::NonZeroU32;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tauri::Emitter;

    /// Глобальный потокобезопасный TorrentManager.
    /// Держит выделенный multi-thread Tokio-рантайм (librqbit требует
    /// полноценный async-контекст), librqbit-сессию и карту хэндлов.
    struct Manager {
        rt: tokio::runtime::Runtime,
        session: Mutex<Option<Arc<Session>>>,
        handles: Mutex<HashMap<String, Arc<librqbit::ManagedTorrent>>>,
        names: Mutex<HashMap<String, String>>,
        magnets: Mutex<HashMap<String, String>>,
        // Сырые байты .torrent для источников-файлов (drag&drop / .torrent).
        files: Mutex<HashMap<String, Vec<u8>>>,
        // Кол-во файлов в торренте (1 = одиночный файл → без подпапки).
        file_counts: Mutex<HashMap<String, usize>>,
        downloads_dir: Mutex<Option<PathBuf>>,
        torrents_dir: Mutex<Option<PathBuf>>,
        // Торренты с отсутствующей на диске папкой — отображаются со статусом
        // "Error" (папку удалили/перенесли вне приложения).
        errored: Mutex<Vec<TorrentStatus>>,
        // info_hash торрентов пакета обновления (раздаются из data/updates) —
        // для бейджа «Обновление» в списке.
        updates: Mutex<HashSet<String>>,
    }

    static MGR: Lazy<Manager> = Lazy::new(|| Manager {
        rt: tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build torrent tokio runtime"),
        session: Mutex::new(None),
        handles: Mutex::new(HashMap::new()),
        names: Mutex::new(HashMap::new()),
        magnets: Mutex::new(HashMap::new()),
        files: Mutex::new(HashMap::new()),
        file_counts: Mutex::new(HashMap::new()),
        downloads_dir: Mutex::new(None),
        torrents_dir: Mutex::new(None),
        errored: Mutex::new(Vec::new()),
        updates: Mutex::new(HashSet::new()),
    });

    static EMITTER_STARTED: AtomicBool = AtomicBool::new(false);

    fn meta_tree() -> sled::Tree {
        crate::DB
            .open_tree("torrents")
            .expect("failed to open 'torrents' sled tree")
    }

    /// Корневая папка загрузок: data/downloads (создаётся при необходимости).
    fn downloads_root(app: &tauri::AppHandle) -> PathBuf {
        let base = crate::ensure_data_dir(app);
        let d = base.join("downloads");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// Кэш системных .torrent: data/torrents/{первые 2 буквы хэша}/{хэш}.torrent
    fn torrents_root(app: &tauri::AppHandle) -> PathBuf {
        let base = crate::ensure_data_dir(app);
        let d = base.join("torrents");
        let _ = std::fs::create_dir_all(&d);
        d
    }

    /// Путь к .torrent-файлу по инфо-хэшу.
    /// Magnet-ссылка торрента: сохранённая либо собранная из info-hash + имени.
    pub(super) fn magnet_of(info_hash: &str) -> String {
        let stored = MGR.magnets.lock().unwrap().get(info_hash).cloned();
        if let Some(m) = stored {
            if m.trim().starts_with("magnet:") {
                return m;
            }
        }
        // Из записи Sled (на случай, если в памяти нет).
        if let Ok(Some(v)) = meta_tree().get(info_hash.as_bytes()) {
            if let Ok(rec) = serde_json::from_slice::<TorrentRecord>(&v) {
                if rec.magnet.trim().starts_with("magnet:") {
                    return rec.magnet;
                }
            }
        }
        // Фолбэк: собираем из хэша, человекочитаемого имени и публичных
        // трекеров. Без `&tr=` удалённый клиент полагается только на DHT,
        // что медленно (иногда — минуты до первого пира) и может вообще
        // не найти сида в фильтрующих сетях. Добавляем до 8 публичных
        // announce, чтобы «холодный» download DevHub-релиза стартовал
        // за секунды даже при одном сидере.
        let name = MGR
            .names
            .lock()
            .unwrap()
            .get(info_hash)
            .cloned()
            .unwrap_or_default();
        let dn = pct_encode(&name);
        let mut magnet = format!("magnet:?xt=urn:btih:{}&dn={}", info_hash.to_lowercase(), dn);
        for tr in public_trackers().iter().take(8) {
            magnet.push_str("&tr=");
            magnet.push_str(&pct_encode(tr));
        }
        magnet
    }

    /// Минимальный percent-encode для display-name magnet-ссылки (без внешних
    /// зависимостей): unreserved-символы как есть, остальное — %XX.
    fn pct_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for &b in s.as_bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    /// Разобрать magnet-URI на info_hash + display-name БЕЗ обращения к сети.
    /// Возвращает `(info_hash_uppercase, dn_or_empty)`. Info-hash обязателен —
    /// иначе `None`. Читается только префикс `xt=urn:btih:<40hex>`; base32
    /// (32 симв.) не поддерживаем — все наши релизы v1.62+ используют hex.
    pub(super) fn peek_magnet(magnet: &str) -> Option<(String, String)> {
        // xt param
        let mut info_hash: Option<String> = None;
        let mut dn = String::new();
        let after = magnet.strip_prefix("magnet:?").unwrap_or(magnet);
        for pair in after.split('&') {
            let mut it = pair.splitn(2, '=');
            let (k, v) = match (it.next(), it.next()) {
                (Some(k), Some(v)) => (k, v),
                _ => continue,
            };
            if k == "xt" {
                if let Some(rest) = v.strip_prefix("urn:btih:") {
                    if rest.len() == 40 && rest.chars().all(|c| c.is_ascii_hexdigit()) {
                        info_hash = Some(rest.to_ascii_uppercase());
                    }
                }
            } else if k == "dn" {
                dn = pct_decode(v);
            }
        }
        info_hash.map(|ih| (ih, dn))
    }

    /// Минимальный percent-decode (для чтения &dn= из магнета).
    fn pct_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = &s[i + 1..i + 3];
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// БЫСТРЫЙ старт magnet-download БЕЗ list-only-блокировки: извлекаем
    /// info_hash и dn regex-ом, кладём в MGR.magnets/names — и сразу
    /// вызываем `start_download`, который передаёт magnet в
    /// `session.add_torrent()` уже в download-режиме. Метаданные подтянутся
    /// в фоне самим librqbit во время загрузки. Возвращает `(info_hash, dn)`
    /// для мгновенного UI-фидбэка.
    ///
    /// Ранее клиент делал: `parse_magnet(list_only)` (БЛОКИРОВКА на
    /// метаданных, могло висеть 10+ минут при холодном DHT) → потом
    /// `start_download`. Пользователь видел «клик не сработал», хотя
    /// торрент фактически стартовал позже. Fast-path решает.
    pub(super) fn quick_start_magnet(
        app: &tauri::AppHandle,
        magnet: String,
    ) -> Result<(String, String), String> {
        let (info_hash, dn) = peek_magnet(&magnet)
            .ok_or_else(|| "magnet-URI без валидного xt=urn:btih:<40hex>".to_string())?;
        // Если уже добавлен — просто возвращаем существующий info_hash.
        if MGR.handles.lock().unwrap().contains_key(&info_hash) {
            return Ok((info_hash, dn));
        }
        // Сохраняем контекст ДО start_download (он читает MGR.magnets/names).
        MGR.magnets
            .lock()
            .unwrap()
            .insert(info_hash.clone(), magnet.clone());
        if !dn.is_empty() {
            MGR.names
                .lock()
                .unwrap()
                .insert(info_hash.clone(), dn.clone());
        }
        // По умолчанию считаем ассет release-single-file (наш кейс). Как только
        // librqbit подтянет метаданные, `snapshot_all` обновит file_counts.
        MGR.file_counts
            .lock()
            .unwrap()
            .entry(info_hash.clone())
            .or_insert(1);
        start_download(app, info_hash.clone(), None, false, None)?;
        Ok((info_hash, dn))
    }

    /// .torrent-файл в base64: из кэша data/torrents либо из метаданных сессии.
    pub(super) fn torrent_file_b64(info_hash: &str) -> Option<String> {
        use base64::Engine;
        // 1) Кэшированный файл.
        if let Some(p) = torrent_file_path(info_hash) {
            if let Ok(bytes) = std::fs::read(&p) {
                if !bytes.is_empty() {
                    return Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
                }
            }
        }
        // 2) Из активного хэндла (metadata → torrent_bytes).
        let handle = MGR.handles.lock().unwrap().get(info_hash).cloned();
        if let Some(h) = handle {
            if let Some(meta) = h.metadata.load_full() {
                let bytes = meta.torrent_bytes.to_vec();
                if !bytes.is_empty() {
                    // Заодно кэшируем на диск.
                    save_torrent_file(info_hash, &bytes);
                    return Some(base64::engine::general_purpose::STANDARD.encode(&bytes));
                }
            }
        }
        None
    }

    fn torrent_file_path(hash: &str) -> Option<PathBuf> {
        let root = MGR.torrents_dir.lock().unwrap().clone()?;
        let prefix: String = hash.chars().take(2).collect();
        Some(root.join(prefix).join(format!("{hash}.torrent")))
    }

    /// Сохранить .torrent на диск (идемпотентно; ускоряет resume).
    fn save_torrent_file(hash: &str, bytes: &[u8]) {
        if let Some(path) = torrent_file_path(hash) {
            if path.exists() {
                return;
            }
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, bytes);
        }
    }

    // --- Публичные BitTorrent-трекеры (ускоряют поиск пиров/метаданных) ---
    // Встроенный фолбэк + периодическое обновление из ngosang/trackerslist.
    const DEFAULT_BT_TRACKERS: &[&str] = &[
        "udp://tracker.opentrackr.org:1337/announce",
        "udp://open.tracker.cl:1337/announce",
        "udp://open.demonii.com:1337/announce",
        "udp://tracker.openbittorrent.com:6969/announce",
        "http://tracker.openbittorrent.com:80/announce",
        "udp://tracker.torrent.eu.org:451/announce",
        "udp://exodus.desync.com:6969/announce",
        "udp://tracker.dler.org:6969/announce",
        "udp://explodie.org:6969/announce",
        "udp://opentracker.i2p.rocks:6969/announce",
        "udp://tracker.moeking.me:6969/announce",
        "udp://tracker.tiny-vps.com:6969/announce",
        "udp://tracker.bittor.pw:1337/announce",
        "udp://tracker-udp.gbitt.info:80/announce",
        "udp://open.stealth.si:80/announce",
    ];
    const BT_TRACKERS_TTL_SECS: u64 = 5 * 24 * 3600;
    // Несколько зеркал источника: если одно недоступно/заблокировано —
    // пробуем следующее. При полном провале остаёмся на списке из БД/фолбэке.
    const BT_TRACKERS_URLS: &[&str] = &[
        "https://raw.githubusercontent.com/ngosang/trackerslist/master/trackers_best.txt",
        "https://cdn.jsdelivr.net/gh/ngosang/trackerslist@master/trackers_best.txt",
        "https://ngosang.github.io/trackerslist/trackers_best.txt",
    ];

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Приватный хранительский ключ RuTracker (`bt`), если задан.
    fn rutracker_key() -> Option<String> {
        crate::DB
            .get(b"cfg:rutracker_bt")
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Announce-URL трекеров RuTracker с подставленным ключом хранителя (`kk`).
    /// Формат берётся из реального keeper-.torrent: `<host>/ann?kk=<key>`.
    fn rutracker_trackers(key: &str) -> Vec<String> {
        let hosts = [
            "bt.t-ru.org",
            "bt2.t-ru.org",
            "bt3.t-ru.org",
            "bt4.t-ru.org",
            "bt.rutracker.cx",
            "bt2.rutracker.cx",
            "bt3.rutracker.cx",
            "bt4.rutracker.cx",
        ];
        hosts
            .iter()
            .map(|h| format!("http://{h}/ann?kk={key}"))
            .collect()
    }

    /// Актуальный список трекеров: из БД (свежий) либо встроенный фолбэк,
    /// плюс приватные announce RuTracker, если задан ключ хранителя.
    fn load_bt_trackers() -> Vec<String> {
        let mut list: Vec<String> = None
            .or_else(|| {
                crate::DB.get(b"cfg:bt_trackers").ok().flatten().and_then(|v| {
                    String::from_utf8(v.to_vec()).ok().map(|s| {
                        s.lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                    })
                })
            })
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| DEFAULT_BT_TRACKERS.iter().map(|s| s.to_string()).collect());
        if let Some(key) = rutracker_key() {
            list.extend(rutracker_trackers(&key));
        }
        list
    }

    /// Только публичные BT-трекеры (кэш из БД либо встроенный фолбэк), без
    /// приватных announce RuTracker — для предзаполнения формы создания торрента.
    pub(super) fn public_trackers() -> Vec<String> {
        crate::DB
            .get(b"cfg:bt_trackers")
            .ok()
            .flatten()
            .and_then(|v| {
                String::from_utf8(v.to_vec()).ok().map(|s| {
                    s.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect::<Vec<_>>()
                })
            })
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| DEFAULT_BT_TRACKERS.iter().map(|s| s.to_string()).collect())
    }

    pub(super) fn get_rutracker_key_pub() -> String {
        rutracker_key().unwrap_or_default()
    }

    pub(super) fn set_rutracker_key_pub(key: String) {
        let k = key.trim();
        if k.is_empty() {
            let _ = crate::DB.remove(b"cfg:rutracker_bt");
        } else {
            let _ = crate::DB.insert(b"cfg:rutracker_bt", k.as_bytes());
        }
        let _ = crate::DB.flush();
    }

    fn trackers_stale() -> bool {
        match crate::DB.get(b"cfg:bt_trackers_ts") {
            Ok(Some(v)) => {
                let ts = String::from_utf8_lossy(&v).parse::<u64>().unwrap_or(0);
                now_secs().saturating_sub(ts) > BT_TRACKERS_TTL_SECS
            }
            _ => true,
        }
    }

    /// Принудительное обновление списка трекеров с GitHub (кнопка «Update
    /// trackers» в окне создания торрента). Перебирает зеркала, сохраняет в БД
    /// и возвращает свежий список.
    pub(super) fn refresh_trackers_now() -> Result<Vec<String>, String> {
        let mut last_err = String::from("no sources");
        for url in BT_TRACKERS_URLS {
            match ureq::get(url).timeout(std::time::Duration::from_secs(15)).call() {
                Ok(r) => match r.into_string() {
                    Ok(body) => {
                        let list: Vec<String> = body
                            .lines()
                            .map(|l| l.trim().to_string())
                            .filter(|l| !l.is_empty() && !l.starts_with('#'))
                            .collect();
                        if list.len() >= 3 {
                            let _ = crate::DB
                                .insert(b"cfg:bt_trackers", list.join("\n").into_bytes());
                            let _ = crate::DB
                                .insert(b"cfg:bt_trackers_ts", now_secs().to_string().into_bytes());
                            crate::syslog::log(
                                "net",
                                format!("manually refreshed {} public BT trackers ({url})", list.len()),
                            );
                            return Ok(list);
                        }
                        last_err = format!("{url}: list too short");
                    }
                    Err(e) => last_err = format!("{url}: {e}"),
                },
                Err(e) => last_err = format!("{url}: {e}"),
            }
        }
        Err(last_err)
    }

    /// Фоновое обновление списка публичных трекеров (не чаще раза в 5 дней).
    /// Перебирает зеркала; при недоступности всех — остаёмся на списке из БД
    /// (или встроенном фолбэке), ts не трогаем → повторим при следующем запуске.
    fn maybe_refresh_trackers() {
        if !trackers_stale() {
            return;
        }
        MGR.rt.spawn_blocking(|| {
            for url in BT_TRACKERS_URLS {
                let body = ureq::get(url)
                    .timeout(std::time::Duration::from_secs(15))
                    .call()
                    .ok()
                    .and_then(|r| r.into_string().ok());
                if let Some(body) = body {
                    let list: Vec<String> = body
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .collect();
                    if list.len() >= 3 {
                        let _ =
                            crate::DB.insert(b"cfg:bt_trackers", list.join("\n").into_bytes());
                        let _ = crate::DB
                            .insert(b"cfg:bt_trackers_ts", now_secs().to_string().into_bytes());
                        crate::syslog::log(
                            "net",
                            format!("refreshed {} public BT trackers ({url})", list.len()),
                        );
                        return;
                    }
                }
            }
            crate::syslog::log(
                "net",
                "BT tracker list update unavailable — using cached/DB trackers".to_string(),
            );
        });
    }

    /// Ленивая инициализация librqbit-сессии (одна на всё приложение).
    pub(super) fn ensure_session(app: &tauri::AppHandle) -> Result<Arc<Session>, String> {
        if let Some(s) = MGR.session.lock().unwrap().as_ref() {
            return Ok(s.clone());
        }
        let dir = downloads_root(app);
        let base = crate::ensure_data_dir(app);
        *MGR.downloads_dir.lock().unwrap() = Some(dir.clone());
        *MGR.torrents_dir.lock().unwrap() = Some(torrents_root(app));
        // Настройки сети торрента (порт/UPnP/µTP/лимит пиров) — из Sled.
        let net = load_net_settings();
        // librqbit 9.0: слушатель входящих задаётся через ListenerOptions.
        //  • mode TcpAndUtp — КЛЮЧЕВОЕ: µTP (uTP поверх UDP) нужен, чтобы
        //    достучаться до пиров за CGNAT/NAT (типично для РФ), как qBittorrent.
        //    Без µTP по чистому TCP такие пиры недостижимы → 0 соединений.
        //  • listen_addr порт 0 = случайный (ОС назначит), либо заданный вручную.
        //  • enable_upnp_port_forwarding — проброс порта на роутере (если не CGNAT).
        let listen = ListenerOptions {
            mode: if net.utp {
                ListenerMode::TcpAndUtp
            } else {
                ListenerMode::TcpOnly
            },
            listen_addr: (std::net::Ipv6Addr::UNSPECIFIED, net.listen_port).into(),
            enable_upnp_port_forwarding: net.upnp,
            ..Default::default()
        };
        let opts = SessionOptions {
            persistence: Some(SessionPersistenceConfig::Json {
                folder: Some(base.join("session")),
            }),
            fastresume: true,
            listen: Some(listen),
            // DHT: None полностью отключает децентрализованный поиск пиров.
            dht: if net.dht {
                Some(librqbit::DhtSessionConfig::default())
            } else {
                None
            },
            disable_local_service_discovery: !net.lsd,
            peer_limit: if net.peer_limit > 0 {
                Some(net.peer_limit as usize)
            } else {
                None
            },
            ..Default::default()
        };
        let session = MGR
            .rt
            .block_on(async move { Session::new_with_opts(dir, opts).await })
            .map_err(|e| format!("session: {e}"))?;
        *MGR.session.lock().unwrap() = Some(session.clone());
        apply_limits_to(&session, load_limits());
        start_progress_emitter(app.clone());
        maybe_refresh_trackers();
        resume_persisted(&session);
        // librqbit сам восстанавливает раздачи из своей persistence (fastresume).
        // Синхронизируем MGR.handles/names напрямую из сессии, иначе список в UI
        // будет пустым после перезапуска, даже когда раздачи фактически активны.
        sync_handles_from_session(&session);
        crate::syslog::log(
            "net",
            format!(
                "torrent session initialised (dht={}, lsd={}, utp={}, upnp={}, port={})",
                net.dht,
                net.lsd,
                net.utp,
                net.upnp,
                if net.listen_port == 0 { "auto".to_string() } else { net.listen_port.to_string() }
            ),
        );
        Ok(session)
    }

    /// Наполнить MGR.handles/names торрентами, которые librqbit восстановил из
    /// собственной persistence при старте сессии (источник истины — сама сессия).
    fn sync_handles_from_session(session: &Arc<Session>) {
        // Замыкание with_torrents — Fn, поэтому только собираем данные в локальный
        // Vec, а MGR-мапы обновляем уже снаружи.
        let collected = session.with_torrents(|it| {
            let mut out = Vec::new();
            for (_id, h) in it {
                out.push((h.info_hash().as_string().to_uppercase(), h.clone()));
            }
            out
        });
        let mut handles = MGR.handles.lock().unwrap();
        let mut names = MGR.names.lock().unwrap();
        for (ih, h) in collected {
            if !names.contains_key(&ih) {
                if let Some(n) = h.name() {
                    names.insert(ih.clone(), n);
                }
            }
            handles.entry(ih).or_insert(h);
        }
    }

    /// Восстановить ранее добавленные торренты из Sled при первом старте.
    fn resume_persisted(session: &Arc<Session>) {
        MGR.errored.lock().unwrap().clear();
        for item in meta_tree().iter() {
            let Ok((k, v)) = item else { continue };
            if k.starts_with(b"__") {
                continue;
            }
            let Ok(rec) = serde_json::from_slice::<TorrentRecord>(&v) else { continue };
            let ih = rec.info_hash.clone();
            let name = rec.name.clone();
            let magnet = rec.magnet.clone();
            // Проверяем наличие папки торрента на диске. Если её удалили/перенесли
            // вне приложения — не поднимаем раздачу, показываем статус "Error".
            let folder = if !rec.folder.trim().is_empty() {
                Some(rec.folder.clone())
            } else {
                rec.save_path.clone()
            };
            // Помечаем торрент как пакет обновления, если он раздаётся из
            // data/updates/v<version> — для бейджа «Обновление» в списке.
            if is_update_folder(folder.as_deref()) {
                MGR.updates.lock().unwrap().insert(ih.clone());
            }
            if let Some(f) = folder.as_ref() {
                if !std::path::Path::new(f).exists() {
                    crate::syslog::log(
                        "net",
                        format!("torrent folder missing → error: {} ({})", name, f),
                    );
                    MGR.errored.lock().unwrap().push(TorrentStatus {
                        info_hash: ih.clone(),
                        name: name.clone(),
                        total_bytes: rec.total_bytes,
                        downloaded_bytes: 0,
                        uploaded_bytes: 0,
                        progress: 0.0,
                        state: "Error".to_string(),
                        seeders_connected: 0,
                        peers_connected: 0,
                        download_speed_bytes: 0,
                        upload_speed_bytes: 0,
                        live_tcp: 0,
                        live_utp: 0,
                        is_update: MGR
                            .updates
                            .lock()
                            .map(|s| s.contains(&ih))
                            .unwrap_or(false),
                    });
                    MGR.names.lock().unwrap().insert(ih.clone(), name);
                    continue;
                }
            }
            let session = session.clone();
            let out = rec.save_path.clone();
            let paused = rec.paused;
            // Источник раздачи: ПРЕДПОЧИТАЕМ сохранённый .torrent (мгновенный
            // resume из локальных файлов с верным info_hash, без ожидания
            // метаданных через DHT). Это критично для торрента обновления в
            // data/updates: после перезапуска обновлённого клиента он поднимается
            // и раздаётся так же, как обычный торрент. Магнет — только фолбэк.
            let file_bytes = torrent_file_path(&ih)
                .and_then(|p| std::fs::read(p).ok())
                .filter(|b| !b.is_empty());
            let magnet_for_add = magnet.clone();
            let file_for_add = file_bytes.clone();
            let res = MGR.rt.block_on(async move {
                let add = match file_for_add {
                    Some(b) => AddTorrent::from_bytes(b),
                    None => AddTorrent::from_url(&magnet_for_add),
                };
                session
                    .add_torrent(
                        add,
                        Some(AddTorrentOptions {
                            overwrite: true,
                            output_folder: out,
                            // Восстанавливаем состояние «на паузе» 1:1 (как qBittorrent).
                            paused,
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            });
            if let Ok(resp) = res {
                if let Some(handle) = resp.into_handle() {
                    MGR.handles.lock().unwrap().insert(ih.clone(), handle);
                    MGR.names.lock().unwrap().insert(ih.clone(), name);
                    if !magnet.trim().is_empty() {
                        MGR.magnets.lock().unwrap().insert(ih.clone(), magnet);
                    }
                    if let Some(b) = file_bytes {
                        MGR.files.lock().unwrap().insert(ih, b);
                    }
                }
            }
        }
    }

    /// Фоновый эмиттер: раз в секунду шлёт `torrent-progress` во фронтенд.
    /// Раз в ~10с: (1) сохраняет .torrent подгруженных магнетов на диск,
    /// (2) обратная DHT-синергия — сливает живые узлы librqbit в наш
    /// анти-цензурный bootstrap-кэш.
    fn start_progress_emitter(app: tauri::AppHandle) {
        if EMITTER_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        MGR.rt.spawn(async move {
            let mut tick: u64 = 0;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let list = snapshot_all();
                let _ = app.emit("torrent-progress", &list);
                tick += 1;
                if tick % 10 == 0 {
                    persist_torrent_files();
                    harvest_dht_nodes();
                }
                // Раз в минуту — накапливаем Byte-Hours экономики раздачи.
                if tick % 60 == 0 {
                    accumulate_profile();
                }
            }
        });
    }

    /// Сохранить .torrent для торрентов, у которых уже подгрузились метаданные.
    fn persist_torrent_files() {
        let handles: Vec<(String, Arc<librqbit::ManagedTorrent>)> = MGR
            .handles
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (ih, h) in handles {
            if torrent_file_path(&ih).map(|p| p.exists()).unwrap_or(true) {
                continue;
            }
            if let Some(meta) = h.metadata.load_full() {
                let bytes = meta.torrent_bytes.to_vec();
                if !bytes.is_empty() {
                    save_torrent_file(&ih, &bytes);
                }
            }
        }
    }

    /// Обратная синергия: живые узлы DHT librqbit → наш bootstrap-кэш.
    fn harvest_dht_nodes() {
        let session = match MGR.session.lock().unwrap().as_ref().cloned() {
            Some(s) => s,
            None => return,
        };
        let Some(dht) = session.get_dht() else { return };
        // librqbit 9.0: clone_routing_table убран — читаем узлы обеих таблиц
        // (IPv4 и IPv6) через with_routing_tables.
        let nodes: Vec<String> = dht.with_routing_tables(|v4, v6| {
            v4.iter()
                .chain(v6.iter())
                .map(|n| n.addr().to_string())
                .take(64)
                .collect()
        });
        if !nodes.is_empty() {
            let added = crate::p2p::merge_dht_hints(nodes);
            if added {
                crate::syslog::log("dht", "harvested live DHT nodes from torrent client".to_string());
            }
        }
    }

    // ============================================================
    //  Экономика Byte-Hours (ранг ноды по объёму раздачи × время)
    // ============================================================

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    /// Запись экономики в Sled-дереве "profile_stats" (ключ `current`).
    /// `byte_hours` хранится в байт-часах (байты × часы); отображается в ГиБ·ч.
    #[derive(Debug, Clone, Serialize, Deserialize, Default)]
    struct ProfileStatsRecord {
        #[serde(default)]
        byte_hours: f64,
        #[serde(default)]
        uploaded_bytes: u64,
        // Суммарно скачано байт за всё время (накопительно, не сбрасывается).
        #[serde(default)]
        downloaded_bytes: u64,
        #[serde(default)]
        updated_at: String,
    }

    // Базовая точка для дельты отданных байт (сбрасывается при перезапуске).
    static LAST_UPLOADED: AtomicU64 = AtomicU64::new(0);
    // Базовая точка для дельты скачанных байт (сбрасывается при перезапуске).
    static LAST_DOWNLOADED: AtomicU64 = AtomicU64::new(0);

    /// Сумма скачанных из сети байт по всем активным раздачам за текущую
    /// сессию (`fetched_bytes` монотонно растёт в рамках сессии, обнуляется
    /// при перезапуске — как и upload-счётчик librqbit).
    fn session_fetched_bytes() -> u64 {
        let handles = MGR.handles.lock().unwrap();
        handles
            .values()
            .map(|h| {
                h.stats()
                    .live
                    .as_ref()
                    .map(|l| l.snapshot.fetched_bytes)
                    .unwrap_or(0)
            })
            .sum()
    }

    fn profile_tree() -> sled::Tree {
        crate::DB
            .open_tree("profile_stats")
            .expect("failed to open 'profile_stats' sled tree")
    }

    fn load_profile() -> ProfileStatsRecord {
        profile_tree()
            .get(b"current")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<ProfileStatsRecord>(&v).ok())
            .unwrap_or_default()
    }

    /// Раз в минуту: интегрируем раздаваемый объём по времени и копим отдачу.
    /// byte_hours += (сумма размеров торрентов в Seeding) × (1 мин / 60).
    fn accumulate_profile() {
        let list = snapshot_all();
        let seeding_volume: u64 = list
            .iter()
            .filter(|s| s.state == "Seeding")
            .map(|s| s.total_bytes)
            .sum();
        let uploaded_sum: u64 = list.iter().map(|s| s.uploaded_bytes).sum();
        let mut rec = load_profile();
        rec.byte_hours += seeding_volume as f64 / 60.0;
        // Дельта отдачи: uploaded_bytes монотонно растёт в рамках сессии; при
        // удалении торрента/перезапуске сумма падает — тогда только сдвигаем
        // базовую точку (без вычитания), не задваивая счётчик.
        let last = LAST_UPLOADED.swap(uploaded_sum, Ordering::SeqCst);
        if uploaded_sum > last {
            rec.uploaded_bytes += uploaded_sum - last;
        }
        // Дельта скачивания — по той же схеме, что и отдача.
        let fetched_sum = session_fetched_bytes();
        let last_dl = LAST_DOWNLOADED.swap(fetched_sum, Ordering::SeqCst);
        if fetched_sum > last_dl {
            rec.downloaded_bytes += fetched_sum - last_dl;
        }
        rec.updated_at = crate::now_iso();
        if let Ok(bytes) = serde_json::to_vec(&rec) {
            let _ = profile_tree().insert(b"current", bytes);
            let _ = profile_tree().flush();
        }

        // Награды за раздачу торрентов → разовый бонус к репутации профиля.
        // SuperSeed: 1000 GB (+100), ANCHOR: 5000 GB (+500). Начисляется один раз.
        const GB: u64 = 1024 * 1024 * 1024;
        let addr = crate::load_vault_internal()
            .map(|v| v.address)
            .unwrap_or_default();
        if !addr.is_empty() {
            if rec.uploaded_bytes >= 1000 * GB {
                crate::reputation::grant_once(&addr, "seed_superseed", 100);
            }
            if rec.uploaded_bytes >= 5000 * GB {
                crate::reputation::grant_once(&addr, "seed_anchor", 500);
            }
        }
    }

    /// Снимок экономики для UI: ГиБ·ч, ранг (по объёму раздачи), отдача, прогресс.
    pub(super) fn profile_stats() -> super::ProfileStats {
        let rec = load_profile();
        let gib = rec.byte_hours / GIB;
        // Ранги по суммарной РАЗДАЧЕ (GB): Leech <10, Peer 10, Seeder 500,
        // SuperSeed 1000 (+100 реп.), Anchor 5000 (+500 реп.).
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        let up_gb = rec.uploaded_bytes as f64 / GB;
        let (rank, lo, next): (&str, f64, Option<f64>) = if up_gb < 10.0 {
            ("Leech", 0.0, Some(10.0))
        } else if up_gb < 500.0 {
            ("Peer", 10.0, Some(500.0))
        } else if up_gb < 1000.0 {
            ("Seeder", 500.0, Some(1000.0))
        } else if up_gb < 5000.0 {
            ("SuperSeed", 1000.0, Some(5000.0))
        } else {
            ("Anchor", 5000.0, None)
        };
        let progress = match next {
            Some(hi) => (((up_gb - lo) / (hi - lo)) as f32).clamp(0.0, 1.0),
            None => 1.0,
        };
        super::ProfileStats {
            byte_hours: gib,
            rank: rank.to_string(),
            uploaded_bytes: rec.uploaded_bytes,
            downloaded_bytes: rec.downloaded_bytes,
            next_rank_at: next,
            progress_to_next: progress,
        }
    }

    fn status_from(info_hash: &str, name: &str, stats: &librqbit::TorrentStats) -> TorrentStatus {
        use librqbit::TorrentStatsState as S;
        let total = stats.total_bytes;
        let done = stats.progress_bytes;
        let progress = if total > 0 {
            (done as f64 / total as f64) as f32
        } else if stats.finished {
            1.0
        } else {
            0.0
        };
        // Состояние: сопоставляем напрямую с типизированным enum librqbit.
        let state = if stats.error.is_some() {
            "Error".to_string()
        } else {
            match stats.state {
                S::Paused => "Paused".to_string(),
                S::Error => "Error".to_string(),
                S::Initializing => "Checking".to_string(),
                S::Live => {
                    if stats.finished {
                        "Seeding".to_string()
                    } else {
                        "Downloading".to_string()
                    }
                }
            }
        };
        // Скорости/пиры — из типизированной live-статистики (надёжнее JSON-навигации).
        // `download_speed.mbps` измеряется в МиБ/с (f64) → переводим в байт/с,
        // сохраняя дробную часть (иначе скорости < 1 МиБ/с обрезались в 0).
        let (dl_bytes, ul_bytes, peers, seeders, live_tcp, live_utp) = match &stats.live {
            Some(live) => {
                let dl = (live.download_speed.mbps * 1_048_576.0).round() as u64;
                let ul = (live.upload_speed.mbps * 1_048_576.0).round() as u64;
                let ps = &live.snapshot.peer_stats;
                (
                    dl,
                    ul,
                    ps.live as u32,
                    ps.seen as u32,
                    ps.live_tcp as u32,
                    ps.live_utp as u32,
                )
            }
            None => (0, 0, 0, 0, 0, 0),
        };
        TorrentStatus {
            info_hash: info_hash.to_string(),
            name: name.to_string(),
            total_bytes: total,
            downloaded_bytes: done,
            uploaded_bytes: stats.uploaded_bytes,
            progress,
            state,
            seeders_connected: seeders,
            peers_connected: peers,
            download_speed_bytes: dl_bytes,
            upload_speed_bytes: ul_bytes,
            live_tcp,
            live_utp,
            is_update: MGR
                .updates
                .lock()
                .map(|s| s.contains(info_hash))
                .unwrap_or(false),
        }
    }

    fn snapshot_all() -> Vec<TorrentStatus> {
        let handles = MGR.handles.lock().unwrap();
        let names = MGR.names.lock().unwrap();
        handles
            .iter()
            .map(|(ih, h)| {
                let stats = h.stats();
                let name = names.get(ih).cloned().unwrap_or_else(|| ih.clone());
                status_from(ih, &name, &stats)
            })
            .collect()
    }

    // Построить TorrentMetadata из list-only ответа librqbit.
    fn extract_meta(lo: librqbit::ListOnlyResponse) -> TorrentMetadata {
        // Id20 сериализуется как байты (не строка) — берём hex через as_string().
        // Верхний регистр (как в magnet btih / на rutracker).
        let info_hash = lo.info_hash.as_string().to_uppercase();
        // librqbit 9.0: lo.info — ValidatedTorrentMetaV1Info; исходные поля
        // (name/files/length) достаём через .info().
        let info = lo.info.info();
        let name = info
            .name
            .as_ref()
            .map(|n| String::from_utf8_lossy(n.as_ref()).to_string())
            .unwrap_or_else(|| info_hash.clone());
        let mut files = Vec::new();
        let mut total: u64 = 0;
        if let Some(flist) = info.files.as_ref() {
            for f in flist {
                let parts: Vec<String> = f
                    .path
                    .iter()
                    .map(|p| String::from_utf8_lossy(p.as_ref()).to_string())
                    .collect();
                let path = if parts.is_empty() {
                    name.clone()
                } else {
                    parts.join("/")
                };
                total += f.length;
                files.push(TorrentFile {
                    path,
                    size: f.length,
                });
            }
        } else if let Some(len) = info.length {
            total = len;
            files.push(TorrentFile {
                path: name.clone(),
                size: len,
            });
        }
        TorrentMetadata {
            info_hash,
            name,
            total_bytes: total,
            files,
        }
    }

    pub(super) fn parse_magnet(
        app: &tauri::AppHandle,
        magnet: String,
    ) -> Result<TorrentMetadata, String> {
        let session = ensure_session(app)?;
        let magnet_for_add = magnet.clone();
        let resp = MGR
            .rt
            .block_on(async move {
                session
                    .add_torrent(
                        AddTorrent::from_url(&magnet_for_add),
                        Some(AddTorrentOptions {
                            list_only: true,
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            })
            .map_err(|e| format!("metadata: {e}"))?;
        let lo = match resp {
            AddTorrentResponse::ListOnly(lo) => lo,
            _ => return Err("ожидался list-only ответ".to_string()),
        };
        let meta = extract_meta(lo);
        MGR.magnets
            .lock()
            .unwrap()
            .insert(meta.info_hash.clone(), magnet.clone());
        MGR.file_counts
            .lock()
            .unwrap()
            .insert(meta.info_hash.clone(), meta.files.len());
        MGR.names
            .lock()
            .unwrap()
            .insert(meta.info_hash.clone(), meta.name.clone());
        Ok(meta)
    }

    /// Распарсить .torrent-файл (drag&drop). Кэширует байты + пишет их на диск.
    pub(super) fn parse_torrent_bytes(
        app: &tauri::AppHandle,
        bytes: Vec<u8>,
    ) -> Result<TorrentMetadata, String> {
        let session = ensure_session(app)?;
        let bytes_for_add = bytes.clone();
        let resp = MGR
            .rt
            .block_on(async move {
                session
                    .add_torrent(
                        AddTorrent::from_bytes(bytes_for_add),
                        Some(AddTorrentOptions {
                            list_only: true,
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            })
            .map_err(|e| format!("metadata: {e}"))?;
        let lo = match resp {
            AddTorrentResponse::ListOnly(lo) => lo,
            _ => return Err("ожидался list-only ответ".to_string()),
        };
        let meta = extract_meta(lo);
        MGR.files
            .lock()
            .unwrap()
            .insert(meta.info_hash.clone(), bytes.clone());
        MGR.names
            .lock()
            .unwrap()
            .insert(meta.info_hash.clone(), meta.name.clone());
        save_torrent_file(&meta.info_hash, &bytes);
        Ok(meta)
    }

    /// Создать .torrent из файла/папки, сохранить его в кэш и сразу поставить
    /// содержимое на раздачу (seeding существующих файлов).
    pub(super) fn create_and_seed(
        app: &tauri::AppHandle,
        source_path: String,
        piece_length: Option<u32>,
        trackers: Vec<String>,
        comment: Option<String>,
        private: bool,
        web_seeds: Vec<String>,
    ) -> Result<TorrentMetadata, String> {
        let session = ensure_session(app)?;
        let path = std::path::PathBuf::from(&source_path);
        if !path.exists() {
            return Err("указанный путь не существует".to_string());
        }
        let is_dir = path.is_dir();
        // Базовая папка (output_folder для librqbit). Явный output_folder
        // отключает авто-подпапку: для папки-раздачи файлы лежат прямо в ней,
        // поэтому base = сам путь; для одиночного файла — родительская папка.
        let seed_base = if is_dir {
            source_path.clone()
        } else {
            path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .ok_or_else(|| "не удалось определить родительскую папку файла".to_string())?
        };
        // Если трекеры не переданы из UI — берём актуальный кэш.
        let trackers = if trackers.iter().all(|t| t.trim().is_empty()) {
            load_bt_trackers()
        } else {
            trackers.into_iter().filter(|t| !t.trim().is_empty()).collect()
        };
        let created_by = format!("SmartNet {}", env!("CARGO_PKG_VERSION"));
        let comment_owned = comment.and_then(|c| {
            let t = c.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        });

        // 1) Генерация .torrent: хэширование содержимого в blocking-контексте.
        let (info_hash, name_str, total_bytes, torrent_bytes) = MGR
            .rt
            .block_on(async move {
                let spawner = librqbit::spawn_utils::BlockingSpawner::new(1);
                let opts = librqbit::CreateTorrentOptions {
                    name: None,
                    trackers,
                    piece_length,
                };
                let mut res = librqbit::create_torrent(&path, opts, &spawner)
                    .await
                    .map_err(|e| format!("создание торрента: {e}"))?;
                // created_by/comment лежат вне info-словаря → не меняют info-hash.
                res.meta.created_by = Some(created_by.as_bytes().into());
                if let Some(c) = comment_owned.as_ref() {
                    res.meta.comment = Some(c.as_bytes().into());
                }
                // Приватный флаг живёт внутри info-словаря → меняет info-hash.
                // WithRawBytes сериализует именно data, поэтому as_bytes() учтёт
                // private=1; хэш затем пересчитываем повторным разбором.
                if private {
                    res.meta.info.data.private = true;
                }
                let info = &res.meta.info.data;
                let name = info
                    .name
                    .as_ref()
                    .map(|n| String::from_utf8_lossy(n.as_ref()).to_string())
                    .unwrap_or_default();
                let total: u64 = if let Some(files) = info.files.as_ref() {
                    files.iter().map(|f| f.length).sum()
                } else {
                    info.length.unwrap_or(0)
                };
                let bytes = res
                    .as_bytes()
                    .map_err(|e| format!("сериализация .torrent: {e}"))?
                    .to_vec();
                // Пересчёт info-hash из финальных байтов (учитывает private).
                let parsed = librqbit::torrent_from_bytes(&bytes)
                    .map_err(|e| format!("повторный разбор .torrent: {e}"))?;
                let ih = parsed.info_hash.as_string().to_uppercase();
                let name = if name.is_empty() { ih.clone() } else { name };
                Ok::<_, String>((ih, name, total, bytes))
            })?;

        // 2) Сохраняем .torrent в кэш (data/torrents/xx/<hash>.torrent).
        save_torrent_file(&info_hash, &torrent_bytes);

        // 2a) Собираем полноценный magnet-URI с публичными трекерами и
        // сохраняем его как канонический источник для этого info_hash. Это
        // критично для DevHub-релизов: удалённый клиент получит его как есть
        // в подписанном DevAsset.magnet и сможет найти пиров через announce,
        // не завися от DHT-медленного стартового rendezvous.
        let mut magnet_uri = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            info_hash.to_lowercase(),
            pct_encode(&name_str)
        );
        for tr in public_trackers().iter().take(8) {
            magnet_uri.push_str("&tr=");
            magnet_uri.push_str(&pct_encode(tr));
        }
        // BEP19 web-seed: HTTP(S)-зеркала с теми же файлами (опционально).
        for ws in web_seeds.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            magnet_uri.push_str("&ws=");
            magnet_uri.push_str(&pct_encode(ws));
        }

        // 3) Добавляем в сессию для раздачи существующих файлов.
        let add_bytes = torrent_bytes.clone();
        let out_for_add = seed_base.clone();
        let resp = MGR
            .rt
            .block_on(async move {
                session
                    .add_torrent(
                        AddTorrent::from_bytes(add_bytes),
                        Some(AddTorrentOptions {
                            overwrite: true,
                            output_folder: Some(out_for_add),
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            })
            .map_err(|e| format!("постановка на раздачу: {e}"))?;
        let handle = resp
            .into_handle()
            .ok_or_else(|| "не удалось получить хэндл раздачи".to_string())?;
        MGR.handles
            .lock()
            .unwrap()
            .insert(info_hash.clone(), handle);
        MGR.names
            .lock()
            .unwrap()
            .insert(info_hash.clone(), name_str.clone());
        MGR.magnets
            .lock()
            .unwrap()
            .insert(info_hash.clone(), magnet_uri.clone());
        MGR.files
            .lock()
            .unwrap()
            .insert(info_hash.clone(), torrent_bytes.clone());
        // Одиночный файл / несколько файлов — для корректной подпапки при
        // ре-скачивании этой же раздачи из UI (см. start_download).
        MGR.file_counts
            .lock()
            .unwrap()
            .insert(info_hash.clone(), if is_dir { 2 } else { 1 });

        // 4) Persist для resume после перезапуска.
        let rec = TorrentRecord {
            info_hash: info_hash.clone(),
            name: name_str.clone(),
            total_bytes,
            magnet: magnet_uri.clone(),
            save_path: Some(seed_base.clone()),
            folder: source_path.clone(),
            paused: false,
            sequential: false,
            added_at: crate::now_iso(),
        };
        if let Ok(bytes) = serde_json::to_vec(&rec) {
            let _ = meta_tree().insert(info_hash.as_bytes(), bytes);
            let _ = meta_tree().flush();
        }
        crate::syslog::log("net", format!("torrent created & seeding: {}", info_hash));

        Ok(TorrentMetadata {
            info_hash,
            name: name_str,
            total_bytes,
            files: Vec::new(),
        })
    }

    /// Безопасное имя папки из названия торрента: убираем разделители пути и
    /// запрещённые в Windows символы, чтобы каждый торрент лёг в свою подпапку.
    fn sanitize_folder_name(name: &str) -> String {
        let cleaned: String = name
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                c if (c as u32) < 0x20 => '_',
                c => c,
            })
            .collect();
        let trimmed = cleaned.trim().trim_matches('.').trim();
        if trimmed.is_empty() {
            "torrent".to_string()
        } else {
            trimmed.to_string()
        }
    }

    pub(super) fn start_download(
        app: &tauri::AppHandle,
        info_hash: String,
        download_path: Option<String>,
        sequential: bool,
        only_files: Option<Vec<usize>>,
    ) -> Result<(), String> {
        let session = ensure_session(app)?;
        // Источник: сохранённый .torrent (drag&drop) либо magnet.
        let file_bytes = MGR.files.lock().unwrap().get(&info_hash).cloned();
        let magnet = MGR.magnets.lock().unwrap().get(&info_hash).cloned();
        if file_bytes.is_none() && magnet.is_none() {
            return Err("источник не найден — сначала вызовите parse_magnet_link/parse_torrent_file".to_string());
        }
        let name = MGR
            .names
            .lock()
            .unwrap()
            .get(&info_hash)
            .cloned()
            .unwrap_or_else(|| info_hash.clone());
        // Базовая директория: явный путь из UI либо корень загрузок.
        let base = download_path
            .filter(|p| !p.trim().is_empty())
            .or_else(|| {
                MGR.downloads_dir
                    .lock()
                    .unwrap()
                    .clone()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .ok_or_else(|| "директория загрузок не инициализирована".to_string())?;
        // Каждый торрент — в собственной подпапке по его названию. Явный
        // output_folder в librqbit отключает авто-подпапку, поэтому добавляем её
        // сами (иначе все файлы валятся прямо в downloads/). Исключение — торрент
        // из ОДНОГО файла: подпапка не нужна, кладём файл прямо в корень загрузок.
        let single_file = MGR
            .file_counts
            .lock()
            .unwrap()
            .get(&info_hash)
            .copied()
            == Some(1);
        let out = if single_file {
            base.clone()
        } else {
            std::path::Path::new(&base)
                .join(sanitize_folder_name(&name))
                .to_string_lossy()
                .to_string()
        };
        let magnet_for_add = magnet.clone();
        let file_for_add = file_bytes.clone();
        let out_for_add = out.clone();
        let only_for_add = only_files.clone();
        let resp = MGR
            .rt
            .block_on(async move {
                let add = match file_for_add {
                    Some(b) => AddTorrent::from_bytes(b),
                    None => AddTorrent::from_url(magnet_for_add.as_deref().unwrap_or("")),
                };
                session
                    .add_torrent(
                        add,
                        Some(AddTorrentOptions {
                            overwrite: true,
                            output_folder: Some(out_for_add),
                            only_files: only_for_add,
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            })
            .map_err(|e| format!("start: {e}"))?;
        let handle = resp
            .into_handle()
            .ok_or_else(|| "не удалось получить хэндл торрента".to_string())?;
        MGR.handles
            .lock()
            .unwrap()
            .insert(info_hash.clone(), handle);
        // Persist в Sled-дерево "torrents" для resume.
        let rec = TorrentRecord {
            info_hash: info_hash.clone(),
            name,
            total_bytes: 0,
            magnet: magnet.unwrap_or_default(),
            save_path: Some(out.clone()),
            folder: out,
            paused: false,
            sequential,
            added_at: crate::now_iso(),
        };
        if let Ok(bytes) = serde_json::to_vec(&rec) {
            let _ = meta_tree().insert(info_hash.as_bytes(), bytes);
            let _ = meta_tree().flush();
        }
        crate::syslog::log("net", format!("torrent started: {}", info_hash));
        // Последовательная загрузка: фоновый drain-читатель через streaming API
        // librqbit — куски качаются по порядку, видео можно смотреть сразу.
        if sequential {
            spawn_sequential(&info_hash);
        }
        Ok(())
    }

    // ==================== HTTP-стриминг (librqbit streaming API) ====================
    // Локальный HTTP-сервер (127.0.0.1, случайный порт) отдаёт файлы торрента с
    // поддержкой Range — <video>/<audio> в webview играют во время скачивания.
    // Куски приоритизируются вокруг позиции чтения самим librqbit (FileStream).
    static STREAM_PORT: Lazy<Mutex<Option<u16>>> = Lazy::new(|| Mutex::new(None));
    // Фоновые «последовательные» читатели (чекбокс Sequential в модалке):
    // дрейнят файлы по порядку через FileStream → librqbit качает куски линейно.
    static SEQ_TASKS: Lazy<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    fn mime_of(name: &str) -> &'static str {
        match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
            "mp4" | "m4v" => "video/mp4",
            "webm" => "video/webm",
            "mkv" => "video/x-matroska",
            "avi" => "video/x-msvideo",
            "mov" => "video/quicktime",
            "ts" => "video/mp2t",
            "mp3" => "audio/mpeg",
            "m4a" | "aac" => "audio/mp4",
            "flac" => "audio/flac",
            "ogg" | "oga" => "audio/ogg",
            "wav" => "audio/wav",
            _ => "application/octet-stream",
        }
    }

    /// Файлы активного торрента (для выбора медиафайла в плеере).
    pub(super) fn list_files(info_hash: &str) -> Result<Vec<super::TorrentFile>, String> {
        let handle = MGR
            .handles
            .lock()
            .unwrap()
            .get(info_hash)
            .cloned()
            .ok_or_else(|| "торрент не найден".to_string())?;
        handle
            .with_metadata(|m| {
                m.file_infos
                    .iter()
                    .map(|fi| super::TorrentFile {
                        path: fi.relative_filename.to_string_lossy().to_string(),
                        size: fi.len,
                    })
                    .collect()
            })
            .map_err(|e| format!("метаданные торрента ещё не готовы: {e}"))
    }

    /// URL стрима файла торрента (лениво поднимает loopback HTTP-сервер).
    pub(super) fn stream_url(info_hash: &str, file_idx: usize) -> Result<String, String> {
        let port = ensure_stream_server()?;
        Ok(format!("http://127.0.0.1:{port}/stream/{info_hash}/{file_idx}"))
    }

    fn ensure_stream_server() -> Result<u16, String> {
        let mut guard = STREAM_PORT.lock().unwrap();
        if let Some(p) = *guard {
            return Ok(p);
        }
        let listener = MGR
            .rt
            .block_on(async { tokio::net::TcpListener::bind("127.0.0.1:0").await })
            .map_err(|e| format!("stream server bind: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| e.to_string())?
            .port();
        MGR.rt.spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((sock, _)) => {
                        tokio::spawn(async move {
                            let _ = handle_stream_conn(sock).await;
                        });
                    }
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
        });
        *guard = Some(port);
        crate::syslog::log("net", format!("torrent stream server: 127.0.0.1:{port}"));
        Ok(port)
    }

    async fn handle_stream_conn(mut sock: tokio::net::TcpStream) -> Result<(), String> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
        // Заголовки запроса: до пустой строки, максимум 8 КБ.
        let mut buf = Vec::with_capacity(2048);
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
                break;
            }
        }
        let req = String::from_utf8_lossy(&buf).to_string();
        let mut lines = req.lines();
        let first = lines.next().unwrap_or("");
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        // Range: bytes=<start>-[<end>]
        let mut range: Option<(u64, Option<u64>)> = None;
        for l in lines {
            let ll = l.to_ascii_lowercase();
            if let Some(v) = ll.strip_prefix("range:") {
                let v = v.trim().strip_prefix("bytes=").unwrap_or("").to_string();
                let mut it = v.splitn(2, '-');
                let s = it.next().unwrap_or("").trim().parse::<u64>().ok();
                let e = it.next().unwrap_or("").trim().parse::<u64>().ok();
                if let Some(s) = s {
                    range = Some((s, e));
                }
            }
        }
        let clean = path.split('?').next().unwrap_or("").trim_start_matches('/');
        let mut segs = clean.split('/');
        let (Some("stream"), Some(hash), Some(idx)) = (segs.next(), segs.next(), segs.next())
        else {
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
                .await;
            return Ok(());
        };
        let file_idx: usize = idx.parse().map_err(|_| "bad file idx".to_string())?;
        let handle = MGR.handles.lock().unwrap().get(hash).cloned();
        let Some(handle) = handle else {
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
                .await;
            return Ok(());
        };
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            handle.wait_until_initialized(),
        )
        .await;
        let info = handle
            .with_metadata(|m| {
                m.file_infos
                    .get(file_idx)
                    .map(|fi| (fi.relative_filename.to_string_lossy().to_string(), fi.len))
            })
            .ok()
            .flatten();
        let Some((fname, flen)) = info else {
            let _ = sock
                .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
                .await;
            return Ok(());
        };
        let mime = mime_of(&fname);
        let (start, end) = match range {
            Some((s, e)) => {
                let s = s.min(flen.saturating_sub(1));
                (s, e.map(|e| e.min(flen.saturating_sub(1))).unwrap_or(flen.saturating_sub(1)))
            }
            None => (0, flen.saturating_sub(1)),
        };
        let content_len = if flen == 0 { 0 } else { end - start + 1 };
        let head = if range.is_some() {
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {content_len}\r\nContent-Range: bytes {start}-{end}/{flen}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nAccept-Ranges: bytes\r\nContent-Length: {content_len}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
            )
        };
        sock.write_all(head.as_bytes()).await.map_err(|e| e.to_string())?;
        if method == "HEAD" || content_len == 0 {
            return Ok(());
        }
        // FileStream librqbit: читает готовые куски, ждёт недокачанные и
        // приоритизирует их у позиции чтения (это и есть стриминг во время загрузки).
        let mut stream = handle
            .clone()
            .stream(file_idx)
            .await
            .map_err(|e| format!("stream: {e}"))?;
        if start > 0 {
            stream
                .seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|e| e.to_string())?;
        }
        let mut left = content_len;
        let mut chunk = vec![0u8; 64 * 1024];
        while left > 0 {
            let want = chunk.len().min(left as usize);
            let n = match stream.read(&mut chunk[..want]).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if sock.write_all(&chunk[..n]).await.is_err() {
                break; // клиент закрыл плеер — прекращаем
            }
            left -= n as u64;
        }
        Ok(())
    }

    /// Последовательная загрузка: фоновый drain всех файлов по порядку через
    /// FileStream — librqbit качает куски линейно от начала к концу.
    pub(super) fn spawn_sequential(info_hash: &str) {
        let hash = info_hash.to_string();
        let handle = MGR.handles.lock().unwrap().get(&hash).cloned();
        let Some(handle) = handle else { return };
        stop_sequential(&hash);
        let h2 = hash.clone();
        let task = MGR.rt.spawn(async move {
            use tokio::io::AsyncReadExt;
            let _ = handle.wait_until_initialized().await;
            let n_files = handle
                .with_metadata(|m| m.file_infos.len())
                .unwrap_or(0);
            let mut buf = vec![0u8; 256 * 1024];
            for fid in 0..n_files {
                let Ok(mut s) = handle.clone().stream(fid).await else { continue };
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            }
            crate::syslog::log("net", format!("sequential download complete: {h2}"));
            SEQ_TASKS.lock().unwrap().remove(&h2);
        });
        SEQ_TASKS.lock().unwrap().insert(hash, task);
    }

    pub(super) fn stop_sequential(info_hash: &str) {
        if let Some(t) = SEQ_TASKS.lock().unwrap().remove(info_hash) {
            t.abort();
        }
    }

    /// Флаг sequential из сохранённой записи (для возобновления drain'а).
    pub(super) fn record_sequential(info_hash: &str) -> bool {
        meta_tree()
            .get(info_hash.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<TorrentRecord>(&v).ok())
            .map(|r| r.sequential)
            .unwrap_or(false)
    }

    /// Как `start_download`, но кладёт файлы в ТОЧНУЮ папку (без авто-подпапки по
    /// имени торрента). Используется системой P2P-обновлений (data/updates/v<ver>).
    pub(super) fn start_download_into(
        app: &tauri::AppHandle,
        info_hash: String,
        out_abs: String,
    ) -> Result<(), String> {
        let session = ensure_session(app)?;
        let file_bytes = MGR.files.lock().unwrap().get(&info_hash).cloned();
        let magnet = MGR.magnets.lock().unwrap().get(&info_hash).cloned();
        if file_bytes.is_none() && magnet.is_none() {
            return Err("источник не найден — сначала распарсите magnet".to_string());
        }
        let name = MGR
            .names
            .lock()
            .unwrap()
            .get(&info_hash)
            .cloned()
            .unwrap_or_else(|| info_hash.clone());
        let _ = std::fs::create_dir_all(&out_abs);
        let out = out_abs;
        let magnet_for_add = magnet.clone();
        let file_for_add = file_bytes.clone();
        let out_for_add = out.clone();
        let resp = MGR
            .rt
            .block_on(async move {
                let add = match file_for_add {
                    Some(b) => AddTorrent::from_bytes(b),
                    None => AddTorrent::from_url(magnet_for_add.as_deref().unwrap_or("")),
                };
                session
                    .add_torrent(
                        add,
                        Some(AddTorrentOptions {
                            overwrite: true,
                            output_folder: Some(out_for_add),
                            trackers: Some(load_bt_trackers()),
                            ..Default::default()
                        }),
                    )
                    .await
            })
            .map_err(|e| format!("start: {e}"))?;
        let handle = resp
            .into_handle()
            .ok_or_else(|| "не удалось получить хэндл торрента".to_string())?;
        MGR.handles
            .lock()
            .unwrap()
            .insert(info_hash.clone(), handle);
        let rec = TorrentRecord {
            info_hash: info_hash.clone(),
            name,
            total_bytes: 0,
            magnet: magnet.unwrap_or_default(),
            save_path: Some(out.clone()),
            folder: out,
            paused: false,
            sequential: false,
            added_at: crate::now_iso(),
        };
        if let Ok(bytes) = serde_json::to_vec(&rec) {
            let _ = meta_tree().insert(info_hash.as_bytes(), bytes);
            let _ = meta_tree().flush();
        }
        crate::syslog::log("net", format!("update torrent started: {}", info_hash));
        Ok(())
    }

    /// Распарсить magnet (подгрузка метаданных) и сразу начать загрузку в точную
    /// папку. Возвращает метаданные (в т.ч. info_hash для отслеживания прогресса).
    pub(super) fn download_magnet_to(
        app: &tauri::AppHandle,
        magnet: String,
        out_abs: String,
    ) -> Result<TorrentMetadata, String> {
        let meta = parse_magnet(app, magnet)?;
        start_download_into(app, meta.info_hash.clone(), out_abs.clone())?;
        // Помечаем как пакет обновления (бейдж «Обновление» в списке).
        if is_update_folder(Some(&out_abs)) {
            MGR.updates.lock().unwrap().insert(meta.info_hash.clone());
        }
        // Как можно раньше кэшируем .torrent обновления: пользователь может
        // нажать «Обновить» и перезапустить клиент сразу после скачивания, не
        // дождавшись 10-секундного тика persist_torrent_files. Без кэша resume
        // после установки полагался бы на повторное разрешение магнета через DHT
        // (сидер релиза может быть офлайн) — торрент не поднимался. С кэшем
        // раздача восстанавливается мгновенно из локальных файлов.
        let ih = meta.info_hash.clone();
        MGR.rt.spawn(async move {
            for _ in 0..120 {
                if torrent_file_path(&ih).map(|p| p.exists()).unwrap_or(true) {
                    break;
                }
                let handle = MGR.handles.lock().unwrap().get(&ih).cloned();
                if let Some(h) = handle {
                    if let Some(m) = h.metadata.load_full() {
                        let bytes = m.torrent_bytes.to_vec();
                        if !bytes.is_empty() {
                            save_torrent_file(&ih, &bytes);
                            break;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
        Ok(meta)
    }

    /// Фоновый логгер статуса торрента обновления в системную консоль (scope
    /// "upd"). Раз в 15 с сообщает, получены ли метаданные, сколько сидов/пиров
    /// и прогресс — чтобы видеть, почему на одном узле пакет качается сразу, а на
    /// другом «висит» (нет источника / метаданные не приходят из DHT).
    pub(super) fn log_update_progress(info_hash: String, version: String) {
        MGR.rt.spawn(async move {
            let mut ticks: u64 = 0;
            let mut last_meta = false;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                ticks += 1;
                if ticks > 160 {
                    crate::syslog::log(
                        "upd",
                        format!("v{version}: monitoring stopped by timeout (~40 min)"),
                    );
                    break;
                }
                let handle = MGR.handles.lock().unwrap().get(&info_hash).cloned();
                let Some(h) = handle else {
                    crate::syslog::log(
                        "upd",
                        format!("v{version}: torrent not found in session — monitoring stopped"),
                    );
                    break;
                };
                let st = h.stats();
                let has_meta = st.total_bytes > 0;
                let (peers, seeders, dl_kib) = match &st.live {
                    Some(l) => (
                        l.snapshot.peer_stats.live,
                        l.snapshot.peer_stats.seen,
                        (l.download_speed.mbps * 1024.0) as u64,
                    ),
                    None => (0, 0, 0),
                };
                let dht = dht_nodes();
                if st.finished {
                    crate::syslog::log(
                        "upd",
                        format!("v{version}: package downloaded 100% — seeding enabled ✓"),
                    );
                    break;
                }
                if !has_meta {
                    crate::syslog::log(
                        "upd",
                        format!(
                            "v{version}: metadata NOT yet received — searching source (DHT: {dht} nodes, peers: {peers}, seeds: {seeders}). If it takes long — the release currently has no live seeds.",
                        ),
                    );
                } else {
                    if !last_meta {
                        crate::syslog::log(
                            "upd",
                            format!(
                                "v{version}: metadata received ({:.1} MB) — starting download",
                                st.total_bytes as f64 / 1_048_576.0
                            ),
                        );
                    }
                    let pct = if st.total_bytes > 0 {
                        (st.progress_bytes as f64 / st.total_bytes as f64 * 100.0) as u32
                    } else {
                        0
                    };
                    crate::syslog::log(
                        "upd",
                        format!(
                            "v{version}: downloading {pct}% (seeds {seeders} / peers {peers}, ↓ {dl_kib} KiB/s)"
                        ),
                    );
                }
                last_meta = has_meta;
            }
        });
    }

    /// Пометить торрент как пакет обновления (бейдж «Обновление» в списке).
    pub(super) fn mark_update(ih: &str) {
        MGR.updates.lock().unwrap().insert(ih.to_string());
    }

    /// Путь ведёт в папку пакета обновления (data/updates/v<version>)?
    fn is_update_folder(path: Option<&str>) -> bool {
        match path {
            Some(p) => p.replace('\\', "/").contains("/updates/v"),
            None => false,
        }
    }

    /// Есть ли активный хэндл для данного info_hash.
    pub(super) fn is_managed(info_hash: &str) -> bool {
        MGR.handles
            .lock()
            .map(|h| h.contains_key(info_hash))
            .unwrap_or(false)
    }

    /// Число хэндлов раздач (перепись памяти crashdiag).
    pub(super) fn handle_count() -> usize {
        MGR.handles.lock().map(|h| h.len()).unwrap_or(0)
    }

    pub(super) fn active() -> Vec<TorrentStatus> {
        let mut list = snapshot_all();
        // Добавляем торренты с отсутствующей папкой (статус Error), избегая дублей.
        let errored = MGR.errored.lock().unwrap().clone();
        for e in errored {
            if !list.iter().any(|s| s.info_hash == e.info_hash) {
                list.push(e);
            }
        }
        list
    }

    /// Обновить флаг `paused` в записи Sled (быстрая операция, не блокирует UI).
    fn set_record_paused(info_hash: &str, paused: bool) {
        if let Ok(Some(v)) = meta_tree().get(info_hash.as_bytes()) {
            if let Ok(mut rec) = serde_json::from_slice::<TorrentRecord>(&v) {
                rec.paused = paused;
                if let Ok(bytes) = serde_json::to_vec(&rec) {
                    let _ = meta_tree().insert(info_hash.as_bytes(), bytes);
                    let _ = meta_tree().flush();
                }
            }
        }
    }

    pub(super) fn pause(info_hash: &str) -> Result<(), String> {
        let session = MGR
            .session
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| "сессия не инициализирована".to_string())?;
        let handle = MGR
            .handles
            .lock()
            .unwrap()
            .get(info_hash)
            .cloned()
            .ok_or_else(|| "торрент не найден".to_string())?;
        // НЕ блокируем UI-поток: пауза большого торрента (сброс состояния на
        // диск) может быть долгой — выполняем в фоне на торрент-runtime.
        MGR.rt.spawn(async move {
            let _ = session.pause(&handle).await;
        });
        stop_sequential(info_hash);
        set_record_paused(info_hash, true);
        Ok(())
    }

    pub(super) fn resume(info_hash: &str) -> Result<(), String> {
        let session = MGR
            .session
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| "сессия не инициализирована".to_string())?;
        let handle = MGR
            .handles
            .lock()
            .unwrap()
            .get(info_hash)
            .cloned()
            .ok_or_else(|| "торрент не найден".to_string())?;
        MGR.rt.spawn(async move {
            let _ = session.unpause(&handle).await;
        });
        set_record_paused(info_hash, false);
        if record_sequential(info_hash) {
            spawn_sequential(info_hash);
        }
        Ok(())
    }

    pub(super) fn remove(info_hash: &str, delete_files: bool) -> Result<(), String> {
        let session = MGR
            .session
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .ok_or_else(|| "сессия не инициализирована".to_string())?;
        let handle = MGR.handles.lock().unwrap().remove(info_hash);
        if let Some(handle) = handle {
            let id = handle.id();
            // Удаление из сессии (в т.ч. с диска) — в фоне, чтобы не вешать UI.
            MGR.rt.spawn(async move {
                let _ = session.delete(id.into(), delete_files).await;
            });
        }
        // Чистим наши структуры и запись сразу (быстро).
        MGR.errored
            .lock()
            .unwrap()
            .retain(|s| s.info_hash != info_hash);
        let _ = meta_tree().remove(info_hash.as_bytes());
        let _ = meta_tree().flush();
        MGR.names.lock().unwrap().remove(info_hash);
        MGR.magnets.lock().unwrap().remove(info_hash);
        MGR.files.lock().unwrap().remove(info_hash);
        stop_sequential(info_hash);
        // Удаляем закэшированный .torrent-файл.
        if let Some(p) = torrent_file_path(info_hash) {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }

    /// Снять из сессии все раздачи, чья папка — `dir` (пакет обновления перед
    /// удалением папки). Файлы не трогаем — папку удаляет вызывающий. Возвращает
    /// число снятых раздач. Если сессия ещё не поднята — снимаем только наши
    /// записи; зомби в persistence librqbit добьёт проверка `package_sane`.
    pub(super) fn forget_in_dir(dir: &std::path::Path) -> usize {
        fn norm(p: &str) -> String {
            p.replace('\\', "/").trim_end_matches('/').to_lowercase()
        }
        let target = norm(&dir.to_string_lossy());
        let mut victims = Vec::new();
        for item in meta_tree().iter() {
            let Ok((k, v)) = item else { continue };
            if k.starts_with(b"__") {
                continue;
            }
            let Ok(rec) = serde_json::from_slice::<TorrentRecord>(&v) else { continue };
            let folder = if rec.folder.trim().is_empty() {
                rec.save_path.clone().unwrap_or_default()
            } else {
                rec.folder.clone()
            };
            if !folder.trim().is_empty() && norm(&folder) == target {
                victims.push(rec.info_hash.clone());
            }
        }
        let mut n = 0usize;
        for ih in victims {
            match remove(&ih, false) {
                Ok(()) => n += 1,
                Err(_) => {
                    // сессии ещё нет — хотя бы забываем запись, чтобы не воскрешать
                    let _ = meta_tree().remove(ih.as_bytes());
                    let _ = meta_tree().flush();
                }
            }
            MGR.updates.lock().unwrap().remove(&ih);
        }
        n
    }


    /// Папка торрента на диске. Начиная с 1.7.5 `save_path` уже содержит полный
    /// путь до подпапки торрента (base/<name>), поэтому возвращаем его напрямую;
    /// для старых записей (save_path = корень) дополнительно пробуем base/<name>.
    pub(super) fn folder_of(info_hash: &str) -> Option<PathBuf> {
        let saved = meta_tree()
            .get(info_hash.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<TorrentRecord>(&v).ok())
            .and_then(|rec| {
                let f = rec.folder.trim().to_string();
                if !f.is_empty() {
                    Some(f)
                } else {
                    rec.save_path.filter(|p| !p.trim().is_empty())
                }
            })
            .map(PathBuf::from);
        let base = saved.or_else(|| MGR.downloads_dir.lock().unwrap().clone())?;
        if base.exists() {
            return Some(base);
        }
        // Фолбэк для старого формата записей: base/<name>.
        let name = MGR.names.lock().unwrap().get(info_hash).cloned();
        if let Some(name) = name {
            let candidate = base.join(sanitize_folder_name(&name));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        Some(base)
    }

    // --- Общие ограничения скорости (KiB/s; 0 = безлимит) ---
    // Хранятся в дереве "torrents" под ключом `__limits` как (dl, ul).
    pub(super) fn load_limits() -> (u32, u32) {
        meta_tree()
            .get(b"__limits")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<(u32, u32)>(&v).ok())
            .unwrap_or((0, 0))
    }

    // --- Настройки сети торрента (порт входящих / UPnP / µTP / лимит пиров) ---
    // Хранятся в дереве "torrents" под ключом `__net`. Применяются при создании
    // сессии (следующий запуск), т.к. librqbit задаёт слушатель/порт разово.
    pub(super) fn load_net_settings() -> super::TorrentNetSettings {
        meta_tree()
            .get(b"__net")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice::<super::TorrentNetSettings>(&v).ok())
            .unwrap_or_default()
    }

    pub(super) fn save_net_settings(s: &super::TorrentNetSettings) {
        if let Ok(bytes) = serde_json::to_vec(s) {
            let _ = meta_tree().insert(b"__net", bytes);
            let _ = meta_tree().flush();
        }
    }

    fn apply_limits_to(session: &Arc<Session>, (dl_kib, ul_kib): (u32, u32)) {
        let to_bps = |kib: u32| NonZeroU32::new(kib.saturating_mul(1024));
        session.ratelimits.set_download_bps(to_bps(dl_kib));
        session.ratelimits.set_upload_bps(to_bps(ul_kib));
    }

    pub(super) fn set_limits(dl_kib: u32, ul_kib: u32) {
        if let Ok(bytes) = serde_json::to_vec(&(dl_kib, ul_kib)) {
            let _ = meta_tree().insert(b"__limits", bytes);
            let _ = meta_tree().flush();
        }
        if let Some(session) = MGR.session.lock().unwrap().as_ref() {
            apply_limits_to(session, (dl_kib, ul_kib));
        }
    }

    /// Число узлов в СОБСТВЕННОЙ BitTorrent-DHT-таблице librqbit
    /// (аналог «DHT: N nodes» в qBittorrent).
    pub(super) fn dht_nodes() -> u64 {
        MGR.session
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| {
                s.get_dht().map(|d| {
                    let st = d.stats();
                    (st.routing_table_size + st.routing_table_size_v6) as u64
                })
            })
            .unwrap_or(0)
    }

    /// Анонсировать раздачу dApp в Mainline DHT (BEP5). `get_peers` с
    /// `announce_port` заставляет DHT-walk разослать `announce_peer` ближайшим
    /// узлам под `app_infohash` — сидер становится находим БЕЗ трекера.
    /// Fire-and-forget: гоняем стрим ~20с и дропаем.
    pub(super) fn dht_announce(infohash: [u8; 20], beacon_port: u16) {
        let dht = MGR.session.lock().unwrap().as_ref().and_then(|s| s.get_dht().cloned());
        let Some(dht) = dht else { return };
        MGR.rt.spawn(async move {
            use n0_future::StreamExt;
            let id = librqbit::dht::Id20::new(infohash);
            let mut stream = dht.get_peers(id, Some(beacon_port));
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    item = stream.next() => { if item.is_none() { break; } }
                }
            }
        });
    }

    /// Найти пиров, анонсировавших dApp (BEP5 `get_peers`). Возвращает их IP:port
    /// (дальше — резолв в iroh EndpointId через identity-beacon в `dht_dapp`).
    pub(super) async fn dht_discover(
        infohash: [u8; 20],
        max: usize,
        timeout: std::time::Duration,
    ) -> Vec<std::net::SocketAddr> {
        let dht = MGR.session.lock().unwrap().as_ref().and_then(|s| s.get_dht().cloned());
        let Some(dht) = dht else { return Vec::new() };
        let (tx, rx) = tokio::sync::oneshot::channel();
        MGR.rt.spawn(async move {
            use n0_future::StreamExt;
            let id = librqbit::dht::Id20::new(infohash);
            let mut stream = dht.get_peers(id, None);
            let mut out: Vec<std::net::SocketAddr> = Vec::new();
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    item = stream.next() => match item {
                        Some(addr) => {
                            if !out.contains(&addr) {
                                out.push(addr);
                            }
                            if out.len() >= max {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
            let _ = tx.send(out);
        });
        rx.await.unwrap_or_default()
    }
}

// ============================================================
//  Tauri-команды (тонкие обёртки; реальная логика — в `imp`)
// ============================================================

/// Парсинг magnet-ссылки + подгрузка метаданных (имя, размер, список файлов).
#[tauri::command]
pub(crate) async fn parse_magnet_link(
    app: tauri::AppHandle,
    magnet_url: String,
) -> Result<TorrentMetadata, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::parse_magnet(&app, magnet_url))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, magnet_url);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Парсинг .torrent-файла (drag&drop): подгрузка метаданных + кэш байтов/на диск.
#[tauri::command]
pub(crate) async fn parse_torrent_file(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
) -> Result<TorrentMetadata, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::parse_torrent_bytes(&app, bytes))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, bytes);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Парсинг .torrent по пути (из события drag-drop Tauri).
#[tauri::command]
pub(crate) async fn parse_torrent_path(
    app: tauri::AppHandle,
    path: String,
) -> Result<TorrentMetadata, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            let bytes = std::fs::read(&path).map_err(|e| format!("чтение файла: {e}"))?;
            imp::parse_torrent_bytes(&app, bytes)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, path);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Запуск скачивания по ранее распарсенному info-hash.
#[tauri::command]
pub(crate) async fn start_torrent_download(
    app: tauri::AppHandle,
    info_hash: String,
    download_path: Option<String>,
    sequential: Option<bool>,
    only_files: Option<Vec<usize>>,
) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        let seq = sequential.unwrap_or(false);
        tauri::async_runtime::spawn_blocking(move || {
            imp::start_download(&app, info_hash, download_path, seq, only_files)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, info_hash, download_path, sequential, only_files);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// БЫСТРЫЙ старт magnet: возвращает info_hash + display-name НЕМЕДЛЕННО
/// (regex по magnet-URI, без сетевого ожидания метаданных). Торрент сразу
/// появляется в списке активных, метаданные догрузятся в фоне librqbit.
#[tauri::command]
pub(crate) async fn quick_start_magnet(
    app: tauri::AppHandle,
    magnet: String,
) -> Result<QuickMagnetResult, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            imp::ensure_session(&app)?;
            imp::quick_start_magnet(&app, magnet).map(|(ih, name)| QuickMagnetResult {
                info_hash: ih,
                name,
            })
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, magnet);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Результат `quick_start_magnet`: info_hash + display-name из &dn=.
#[derive(serde::Serialize, Clone)]
pub(crate) struct QuickMagnetResult {
    #[serde(rename = "infoHash")]
    pub info_hash: String,
    pub name: String,
}

/// Инициализировать торрент-сессию и восстановить сохранённые торренты при
/// запуске приложения (в фоне, чтобы не блокировать setup). Без этого сессия
/// поднималась лениво и торренты появлялись только после действия пользователя.
pub(crate) fn resume_on_startup(app: tauri::AppHandle) {
    #[cfg(feature = "p2p")]
    {
        let _ = std::thread::Builder::new().name("torrent-session".into()).spawn(move || {
            if let Err(e) = imp::ensure_session(&app) {
                crate::syslog::log("net", format!("torrent resume on startup failed: {e}"));
            }
        });
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = app;
    }
}

/// Добавить magnet и запустить скачивание (headless, для загрузчика языковой
/// модели). Возвращает info_hash. Блокирующая — вызывать в spawn_blocking.
pub(crate) fn add_and_start_magnet(app: &tauri::AppHandle, magnet: &str) -> Result<String, String> {
    #[cfg(feature = "p2p")]
    {
        imp::ensure_session(app)?;
        let meta = imp::parse_magnet(app, magnet.to_string())?;
        let ih = meta.info_hash.clone();
        imp::start_download(app, ih.clone(), None, false, None)?;
        Ok(ih)
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, magnet);
        Err("Торрент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Снимок статуса конкретного торрента по info_hash (для отслеживания загрузки).
pub(crate) fn status_for(info_hash: &str) -> Option<TorrentStatus> {
    #[cfg(feature = "p2p")]
    {
        imp::active()
            .into_iter()
            .find(|t| t.info_hash.eq_ignore_ascii_case(info_hash))
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        None
    }
}

/// Снять из сессии раздачи, привязанные к папке пакета обновления (перед её
/// удалением). Возвращает число снятых раздач.
pub(crate) fn forget_torrents_in_dir(dir: &std::path::Path) -> usize {
    #[cfg(feature = "p2p")]
    {
        imp::forget_in_dir(dir)
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = dir;
        0
    }
}

/// Папка торрента на диске (куда легли скачанные файлы).
pub(crate) fn folder_for(info_hash: &str) -> Option<std::path::PathBuf> {
    #[cfg(feature = "p2p")]
    {
        imp::folder_of(info_hash)
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        None
    }
}

/// Снимок всех известных торрентов (активные/на паузе/завершённые).
/// async: снимок берёт мьютексы и stats() всех раздач — уводим с main-потока,
/// иначе частый поллинг UI периодически подвешивает все webview.
#[tauri::command]
pub(crate) async fn get_active_torrents(app: tauri::AppHandle) -> Vec<TorrentStatus> {
    #[cfg(feature = "p2p")]
    {
        // НЕ инициализируем сессию здесь: ensure_session при первом вызове делает
        // block_on (создание сессии + resume + сеть). Подъём сессии выполняет
        // resume_on_startup в отдельном потоке при запуске. Здесь — только снимок.
        let _ = app;
        tauri::async_runtime::spawn_blocking(imp::active)
            .await
            .unwrap_or_default()
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = app;
        Vec::new()
    }
}

#[tauri::command]
pub(crate) async fn pause_torrent(info_hash: String) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::pause(&info_hash))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

#[tauri::command]
pub(crate) async fn resume_torrent(info_hash: String) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::resume(&info_hash))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

#[tauri::command]
pub(crate) async fn remove_torrent(info_hash: String, delete_files: Option<bool>) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        let del = delete_files.unwrap_or(false);
        tauri::async_runtime::spawn_blocking(move || imp::remove(&info_hash, del))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (info_hash, delete_files);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Magnet-ссылка активного торрента (для пункта меню «Скопировать magnet»).
#[tauri::command]
pub(crate) async fn get_torrent_magnet(info_hash: String) -> Result<String, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::magnet_of(&info_hash))
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Список файлов активного торрента (для выбора медиафайла в плеере).
#[tauri::command]
pub(crate) async fn torrent_list_files(info_hash: String) -> Result<Vec<TorrentFile>, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::list_files(&info_hash))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// URL локального HTTP-стрима файла торрента (Range-поддержка, играет во
/// время скачивания — куски приоритизируются вокруг позиции просмотра).
#[tauri::command]
pub(crate) async fn torrent_stream_url(info_hash: String, file_idx: usize) -> Result<String, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::stream_url(&info_hash, file_idx))
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (info_hash, file_idx);
        Err("Стриминг доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// .torrent-файл активного торрента в Base64 (для экспорта «Сохранить как»).
#[tauri::command]
pub(crate) async fn get_torrent_file_bytes(info_hash: String) -> Result<String, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::torrent_file_b64(&info_hash))
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "не удалось получить .torrent (метаданные ещё не загружены)".to_string())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Проверить, управляется ли уже торрент с таким info_hash (есть активный хэндл).
/// Используется системой обновлений, чтобы не «качать у самого себя».
#[cfg(feature = "p2p")]
pub(crate) fn is_managed(info_hash: &str) -> bool {
    imp::is_managed(info_hash)
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn is_managed(_info_hash: &str) -> bool {
    false
}

/// Число раздач в сессии (перепись памяти crashdiag).
pub(crate) fn managed_count() -> usize {
    #[cfg(feature = "p2p")]
    {
        imp::handle_count()
    }
    #[cfg(not(feature = "p2p"))]
    {
        0
    }
}

/// Список публичных BT-трекеров для предзаполнения формы создания торрента.
#[tauri::command]
pub(crate) async fn get_bt_trackers() -> Vec<String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(imp::public_trackers)
            .await
            .unwrap_or_default()
    }
    #[cfg(not(feature = "p2p"))]
    {
        Vec::new()
    }
}

/// Принудительно скачать актуальный список публичных трекеров с GitHub.
#[tauri::command]
pub(crate) async fn refresh_bt_trackers() -> Result<Vec<String>, String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(imp::refresh_trackers_now)
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        Err("p2p disabled".into())
    }
}

/// Загрузить magnet в ТОЧНУЮ папку (для системы P2P-обновлений). Возвращает
/// метаданные торрента (в т.ч. info_hash для отслеживания прогресса).
#[cfg(feature = "p2p")]
pub(crate) fn download_magnet_update(
    app: &tauri::AppHandle,
    magnet: String,
    out_abs: String,
) -> Result<TorrentMetadata, String> {
    imp::download_magnet_to(app, magnet, out_abs)
}

/// Добавить торрент обновления из .torrent-байтов (скачанных с HTTP-зеркала).
/// КЛЮЧЕВОЕ отличие от magnet-пути: метаданные уже в руках — НЕ ждём их из
/// DHT/пиров, поэтому обновление работает даже при НУЛЕ живых сидов (файлы
/// докачиваются с web-seed зеркала, торрент верифицирует и раздаёт).
/// info_hash обязан совпасть с btih из блокчейна — иначе пакет отвергается.
#[cfg(feature = "p2p")]
pub(crate) fn add_update_from_bytes(
    app: &tauri::AppHandle,
    bytes: Vec<u8>,
    expect_info_hash: &str,
    out_abs: String,
) -> Result<TorrentMetadata, String> {
    let meta = imp::parse_torrent_bytes(app, bytes)?;
    if !meta.info_hash.eq_ignore_ascii_case(expect_info_hash.trim()) {
        return Err(format!(
            "web-seed .torrent info_hash mismatch: {} != {}",
            meta.info_hash, expect_info_hash
        ));
    }
    imp::mark_update(&meta.info_hash);
    imp::start_download_into(app, meta.info_hash.clone(), out_abs)?;
    Ok(meta)
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn add_update_from_bytes(
    _app: &tauri::AppHandle,
    _bytes: Vec<u8>,
    _expect_info_hash: &str,
    _out_abs: String,
) -> Result<TorrentMetadata, String> {
    Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn download_magnet_update(
    _app: &tauri::AppHandle,
    _magnet: String,
    _out_abs: String,
) -> Result<TorrentMetadata, String> {
    Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
}

/// Запустить фоновый логгер статуса загрузки обновления в системную консоль.
#[cfg(feature = "p2p")]
pub(crate) fn log_update_progress(_app: tauri::AppHandle, info_hash: String, version: String) {
    imp::log_update_progress(info_hash, version);
}

#[cfg(not(feature = "p2p"))]
pub(crate) fn log_update_progress(_app: tauri::AppHandle, _info_hash: String, _version: String) {}

/// Создать .torrent из файла/папки, сохранить в кэш и сразу начать раздачу.
#[tauri::command]
pub(crate) async fn create_torrent_from_path(
    app: tauri::AppHandle,
    source_path: String,
    piece_length: Option<u32>,
    trackers: Vec<String>,
    comment: Option<String>,
    private: Option<bool>,
    web_seeds: Option<Vec<String>>,
) -> Result<TorrentMetadata, String> {
    #[cfg(feature = "p2p")]
    {
        let priv_flag = private.unwrap_or(false);
        let ws = web_seeds.unwrap_or_default();
        tauri::async_runtime::spawn_blocking(move || {
            imp::create_and_seed(&app, source_path, piece_length, trackers, comment, priv_flag, ws)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, source_path, piece_length, trackers, comment, private, web_seeds);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Public accessor: собрать/достать magnet-URI по инфо-хэшу активной раздачи.
/// Используется devhub при создании релиза — сразу после
/// `create_torrent_from_path` берём magnet и кладём в подписанный DevAsset.
pub(crate) fn magnet_from_info_hash(info_hash: &str) -> String {
    #[cfg(feature = "p2p")]
    { imp::magnet_of(info_hash) }
    #[cfg(not(feature = "p2p"))]
    { let _ = info_hash; String::new() }
}

/// Общие ограничения скорости (KiB/s; 0 = без ограничения).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentLimits {
    #[serde(rename = "downloadKib")]
    pub download_kib: u32,
    #[serde(rename = "uploadKib")]
    pub upload_kib: u32,
}

#[tauri::command]
pub(crate) fn get_torrent_limits() -> TorrentLimits {
    #[cfg(feature = "p2p")]
    {
        let (dl, ul) = imp::load_limits();
        TorrentLimits {
            download_kib: dl,
            upload_kib: ul,
        }
    }
    #[cfg(not(feature = "p2p"))]
    {
        TorrentLimits {
            download_kib: 0,
            upload_kib: 0,
        }
    }
}

#[tauri::command]
pub(crate) fn set_torrent_limits(download_kib: u32, upload_kib: u32) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        imp::set_limits(download_kib, upload_kib);
        Ok(())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (download_kib, upload_kib);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

#[tauri::command]
pub(crate) fn get_torrent_dht_nodes() -> u64 {
    #[cfg(feature = "p2p")]
    {
        imp::dht_nodes()
    }
    #[cfg(not(feature = "p2p"))]
    {
        0
    }
}

/// Трекерлесс-анонс раздачи dApp в Mainline DHT (BEP5) под `app_infohash`,
/// указывая порт identity-beacon (там резолвится iroh EndpointId).
pub(crate) fn dht_announce_dapp(_infohash: [u8; 20], _beacon_port: u16) {
    #[cfg(feature = "p2p")]
    {
        imp::dht_announce(_infohash, _beacon_port);
    }
}

/// Трекерлесс-поиск IP:port’ов сидеров dApp в Mainline DHT (BEP5 get_peers).
pub(crate) async fn dht_discover_dapp(
    _infohash: [u8; 20],
    _max: usize,
    _timeout_secs: u64,
) -> Vec<std::net::SocketAddr> {
    #[cfg(feature = "p2p")]
    {
        imp::dht_discover(_infohash, _max, std::time::Duration::from_secs(_timeout_secs)).await
    }
    #[cfg(not(feature = "p2p"))]
    {
        Vec::new()
    }
}

/// Экономика Byte-Hours: суммарный вклад ноды в раздачу и её ранг.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileStats {
    /// Накоплено байт-часов в ГиБ·ч (объём в раздаче × время).
    #[serde(rename = "byteHours")]
    pub byte_hours: f64,
    /// Ранг ноды: Leech | Peer | Seeder | Anchor.
    pub rank: String,
    /// Суммарно отдано байт.
    #[serde(rename = "uploadedBytes")]
    pub uploaded_bytes: u64,
    /// Суммарно скачано байт (накопительно).
    #[serde(rename = "downloadedBytes")]
    pub downloaded_bytes: u64,
    /// Порог следующего ранга в ГиБ·ч (null для максимального ранга Anchor).
    #[serde(rename = "nextRankAt")]
    pub next_rank_at: Option<f64>,
    /// Прогресс до следующего ранга [0..1].
    #[serde(rename = "progressToNext")]
    pub progress_to_next: f32,
}

#[tauri::command]
pub(crate) async fn get_profile_stats() -> ProfileStats {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(imp::profile_stats)
            .await
            .unwrap_or_else(|_| ProfileStats {
                byte_hours: 0.0,
                rank: "Leech".to_string(),
                uploaded_bytes: 0,
                downloaded_bytes: 0,
                next_rank_at: Some(1.0),
                progress_to_next: 0.0,
            })
    }
    #[cfg(not(feature = "p2p"))]
    {
        ProfileStats {
            byte_hours: 0.0,
            rank: "Leech".to_string(),
            uploaded_bytes: 0,
            downloaded_bytes: 0,
            next_rank_at: Some(1.0),
            progress_to_next: 0.0,
        }
    }
}

/// Текущие накопленные Byte-Hours ноды в ГиБ·ч — отправляются на трекер в
/// heartbeat (seeding.rs), чтобы клиент ранжировался на публичном /map-лидерборде.
pub(crate) fn current_byte_hours() -> f64 {
    #[cfg(feature = "p2p")]
    {
        imp::profile_stats().byte_hours
    }
    #[cfg(not(feature = "p2p"))]
    {
        0.0
    }
}

/// Сетевые настройки BitTorrent-соединений (порт входящих / UPnP / µTP / лимит
/// пиров). Применяются при инициализации сессии (после перезапуска приложения).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TorrentNetSettings {
    /// Порт для входящих соединений. 0 = случайный (назначит ОС).
    #[serde(rename = "listenPort", default)]
    pub listen_port: u16,
    /// Проброс порта через UPnP на роутере (бесполезно за CGNAT).
    #[serde(default = "def_true")]
    pub upnp: bool,
    /// Включить µTP (TCP+uTP). Нужно для пиров за CGNAT/NAT, как в qBittorrent.
    #[serde(default = "def_true")]
    pub utp: bool,
    /// Лимит пиров на торрент. 0 = дефолт librqbit.
    #[serde(rename = "peerLimit", default)]
    pub peer_limit: u32,
    /// Включить DHT (децентрализованный поиск пиров).
    #[serde(default = "def_true")]
    pub dht: bool,
    /// Включить LSD (обнаружение локальных пиров).
    #[serde(default = "def_true")]
    pub lsd: bool,
}

fn def_true() -> bool {
    true
}

impl Default for TorrentNetSettings {
    fn default() -> Self {
        Self {
            listen_port: 0,
            upnp: true,
            utp: true,
            peer_limit: 0,
            dht: true,
            lsd: true,
        }
    }
}

#[tauri::command]
pub(crate) async fn get_torrent_net_settings() -> TorrentNetSettings {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(imp::load_net_settings)
            .await
            .unwrap_or_default()
    }
    #[cfg(not(feature = "p2p"))]
    {
        TorrentNetSettings::default()
    }
}

#[tauri::command]
pub(crate) async fn set_torrent_net_settings(settings: TorrentNetSettings) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || imp::save_net_settings(&settings))
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = settings;
        Ok(())
    }
}

#[tauri::command]
pub(crate) fn get_rutracker_key() -> String {
    #[cfg(feature = "p2p")]
    {
        imp::get_rutracker_key_pub()
    }
    #[cfg(not(feature = "p2p"))]
    {
        String::new()
    }
}

#[tauri::command]
pub(crate) fn set_rutracker_key(key: String) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        imp::set_rutracker_key_pub(key);
        Ok(())
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = key;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Открыть папку с загруженным торрентом в файловом менеджере ОС.
#[tauri::command]
pub(crate) async fn open_torrent_folder(info_hash: String) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        tauri::async_runtime::spawn_blocking(move || {
            let path = imp::folder_of(&info_hash).ok_or_else(|| "папка не найдена".to_string())?;
            open_in_file_manager(&path)
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = info_hash;
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

#[cfg(feature = "p2p")]
fn open_in_file_manager(path: &std::path::Path) -> Result<(), String> {
    // Нормализуем разделители (форвард-слэши ломают explorer.exe → открывал «Документы»)
    // и создаём папку, если её ещё нет.
    let _ = std::fs::create_dir_all(path);
    let p = path
        .to_string_lossy()
        .replace('/', std::path::MAIN_SEPARATOR_STR);
    #[cfg(target_os = "windows")]
    let cmd = ("explorer", vec![p]);
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![p]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = ("xdg-open", vec![p]);
    std::process::Command::new(cmd.0)
        .args(cmd.1)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Open a torrent's payload with the system default app. Single-file torrent →
/// opens the file itself (via the OS opener); multi-file → opens the folder.
/// Async + spawn_blocking for the directory walk (must not block the main thread).
#[tauri::command]
pub(crate) async fn open_torrent_file(
    app: tauri::AppHandle,
    info_hash: String,
) -> Result<(), String> {
    #[cfg(feature = "p2p")]
    {
        use tauri_plugin_shell::ShellExt;
        let folder = imp::folder_of(&info_hash).ok_or_else(|| "папка не найдена".to_string())?;
        let folder2 = folder.clone();
        let single = tokio::task::spawn_blocking(move || single_file_in(&folder2))
            .await
            .map_err(|e| e.to_string())?;
        match single {
            Some(file) => app
                .shell()
                .open(file.to_string_lossy().to_string(), None)
                .map_err(|e| e.to_string()),
            None => open_in_file_manager(&folder),
        }
    }
    #[cfg(not(feature = "p2p"))]
    {
        let _ = (app, info_hash);
        Err("Торрент-клиент доступен только в десктоп-сборке (P2P).".to_string())
    }
}

/// Return the single regular file inside `dir` (recursively), or `None` if there
/// are zero or more than one files (multi-file torrent).
#[cfg(feature = "p2p")]
fn single_file_in(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut found: Option<std::path::PathBuf> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                if found.is_some() {
                    return None; // more than one file
                }
                found = Some(p);
            }
        }
    }
    found
}
