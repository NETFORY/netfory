// ============================================================
//  SmartNet // devhub.rs — DevHub: децентрализованный аналог GitHub (Фаза 2).
//
//  Принципы (согласовано с владельцем продукта):
//    - ПОЛНОСТЬЮ БЕСПЛАТНО: публикация репозитория не требует транзакций.
//      Идентичность автора уже куплена один раз — on-chain имя (naming.rs).
//    - Индекс репозиториев автора — подписанный JSON-манифест (legacy
//      bip-schnorr ключом кошелька). Читатель проверяет подпись против
//      адреса, отрезолвленного из блокчейн-реестра имён → подделка невозможна.
//    - Защита от откатов (replay): монотонный счётчик `seq` внутри подписи.
//      Клиент никогда не принимает индекс с seq меньше уже виденного.
//      (Опциональный on-chain чекпоинт версии — следующим шагом.)
//    - Хранилище НЕ грузит Sled: файлы репозиториев лежат на диске в
//      data/devhub/<address>/<repo>/, индекс — data/devhub/<address>/index.json.
//      В Sled — только лёгкий кэш чужих индексов (devhub:remote:<addr>).
//    - Релизы (exe/dmg/apk/deb/snap…) НИКОГДА не хранятся в devhub-папке и
//      не скачиваются автоматически: манифест релиза ссылается на f://<cid>
//      файлообменника, загрузка — только по явному клику (как на GitHub).
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use crate::{load_vault_internal, DB, SESSION_SEED};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Sled-префикс кэша ЧУЖИХ (полученных из сети) подписанных индексов.
pub(crate) const REMOTE_INDEX_PREFIX: &str = "devhub:remote:";
/// Sled-префикс дневной активности ЧУЖИХ авторов (контрибуции из сети).
const REMOTE_ACTIVITY_PREFIX: &str = "devhub:activity:";
/// Максимальный размер текстового файла для просмотрщика кода (2 МБ).
const MAX_TEXT_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Служебные файлы devhub-хранилища — скрыты из просмотрщика кода.
const SERVICE_FILES: &[&str] = &["repo.bundle", "refs.json"];

// ------------------------------------------------------------
//  Модели данных
// ------------------------------------------------------------

/// Файл-ассет релиза: живёт в файлообменнике, доступен как f://<cid>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevAsset {
    pub name: String,
    pub size: u64,
    /// CID файлообменника (f://) — скачивание ТОЛЬКО по явному клику.
    pub cid: String,
    /// magnet:?xt=urn:btih:… — торрент-транспорт релиза. Пусто для
    /// старых релизов (< v1.62). При создании релиза бэкенд автоматически
    /// упаковывает файл в торрент через `torrent::create_torrent_from_path`
    /// и сохраняет magnet-URI сюда. Целостность цепочки гарантируется
    /// подписью автора над всем `IndexRecord` (см. `verify_index`) — magnet
    /// невозможно подделать без приватного ключа разработчика.
    #[serde(default)]
    pub magnet: String,
    /// v1.63.2 Publisher WebSeed (BEP-19): опциональный HTTP(S)-URL зеркала
    /// (GitHub Release / S3 / собственный CDN), с которого можно тянуть
    /// файл, если в BitTorrent-swarm нет живых пиров. librqbit добавит его
    /// как fallback-источник; UI показывает mirror-иконку. Пустая строка
    /// или невалидный URL → mirror игнорируется. Как и magnet, mirror
    /// защищён подписью автора над всем IndexRecord.
    #[serde(default)]
    pub mirror: String,
}

/// Релиз репозитория (аналог GitHub Releases).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevRelease {
    pub tag: String,
    pub title: String,
    pub notes: String,
    pub created: u64,
    #[serde(default)]
    pub assets: Vec<DevAsset>,
}

/// Метаданные одного репозитория внутри подписанного индекса автора.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevRepoMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// SPDX-идентификатор лицензии ("MIT", "Apache-2.0", …) или пусто.
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub language: String,
    pub default_branch: String,
    pub branches: Vec<String>,
    pub created: u64,
    pub updated: u64,
    #[serde(default)]
    pub commits: u32,
    #[serde(default)]
    pub releases: Vec<DevRelease>,
    /// blake3-хэш iroh-blob'а полного git-bundle (для P2P-клонирования).
    #[serde(default)]
    pub bundle_hash: String,
    /// Git-рефы репозитория (подписаны в составе индекса → подделка невозможна).
    #[serde(default)]
    pub refs: Vec<DevRef>,
    /// История коммитов (новые сверху, кап MAX_LOG) — вкладка «Коммиты».
    #[serde(default)]
    pub log: Vec<DevCommit>,
    /// Лёгкое дерево файлов (путь + размер, кап MAX_TREE): чужие клиенты
    /// показывают вкладку «Код» мгновенно из анонса, без скачивания bundle.
    /// skip_serializing_if: пустое дерево не попадает в JSON — старые подписи живы.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tree: Vec<DevTreeEntry>,
    /// «username/repo» источника форка (как «forked from» на GitHub).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub forked_from: String,
}

/// Подписанный индекс всех репозиториев автора. Раздаётся в P2P-сеть;
/// подпись покрывает address + seq + updated + hash(repos).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevUserIndex {
    pub version: u32,
    pub username: String,
    pub address: String,
    /// Compressed secp256k1 pubkey (hex) — проверяется против address.
    #[serde(default)]
    pub pubkey: String,
    /// Монотонный анти-откат счётчик: клиент не принимает seq меньше виденного.
    pub seq: u64,
    pub updated: u64,
    pub repos: Vec<DevRepoMeta>,
    /// ⭐ Подписанные стары: список "username/repo", которые отметил ЭТОТ автор.
    /// Распространяются вместе с индексом → любой пир честно считает рейтинг
    /// (каждая звезда подписана ключом и подкреплена on-chain именем).
    #[serde(default)]
    pub starred: Vec<String>,
    /// blake3-хэш iroh-blob'а prs.json (исходящие Pull Request'ы автора).
    /// Каждый PR внутри подписан отдельно ключом контрибьютора, поэтому
    /// подделка prs_hash даёт максимум мусорный blob — PR не верифицируются.
    #[serde(default)]
    pub prs_hash: String,
    /// id принятых (merged) PR — контрибьютор видит статус из индекса владельца.
    #[serde(default)]
    pub merged_prs: Vec<String>,
    #[serde(default)]
    pub sig: String,
}

/// Элемент списка файлов (вкладка «Код»).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevFileEntry {
    pub name: String,
    /// Путь относительно корня репозитория.
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
}

/// Запись лёгкого дерева файлов в подписанном анонсе (путь + размер).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevTreeEntry {
    pub path: String,
    pub size: u64,
}

/// Кап записей дерева файлов в подписанном индексе (защита размера).
const MAX_TREE: usize = 2000;

/// Содержимое файла для просмотрщика кода.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevFileContent {
    pub content: String,
    pub size: u64,
    pub binary: bool,
    pub truncated: bool,
}

/// Git-реф репозитория (SHA коммита + полное имя ветки/тега).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DevRef {
    pub sha: String,
    pub name: String,
}

/// Один коммит истории (лог хранится в подписанном индексе, кап 50).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevCommit {
    pub sha: String,
    pub message: String,
    pub author: String,
    /// unix-секунды коммита.
    pub date: u64,
}

/// Максимум коммитов в логе метаданных (защита размера индекса).
pub(crate) const MAX_LOG: usize = 50;

/// Событие дня для графа контрибуций («какие репо обновлялись»).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DevEvent {
    pub repo: String,
    /// created | push | deleted | release
    pub action: String,
    /// Сообщение коммита (для push) — показывается в событиях профиля.
    #[serde(default)]
    pub msg: String,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------
//  Дисковое хранилище: data/devhub/<address>/<repo>/
// ------------------------------------------------------------

pub(crate) fn devhub_root(app: &tauri::AppHandle) -> PathBuf {
    crate::ensure_data_dir(app).join("devhub")
}

pub(crate) fn user_dir(app: &tauri::AppHandle, address: &str) -> PathBuf {
    devhub_root(app).join(address)
}

pub(crate) fn repo_dir(app: &tauri::AppHandle, address: &str, repo: &str) -> PathBuf {
    user_dir(app, address).join(repo)
}

fn index_path(app: &tauri::AppHandle, address: &str) -> PathBuf {
    user_dir(app, address).join("index.json")
}

/// Публичный путь к index.json (нужен циклу сидинга p2p для re-seed после рестарта).
pub(crate) fn index_path_for(app: &tauri::AppHandle, address: &str) -> PathBuf {
    index_path(app, address)
}

/// Безопасное соединение путей: запрещает `..`, абсолютные пути и префиксы —
/// защита от path traversal при просмотре файлов чужих репозиториев.
fn safe_join(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut out = root.to_path_buf();
    for comp in Path::new(rel).components() {
        match comp {
            std::path::Component::Normal(c) => out.push(c),
            std::path::Component::CurDir => {}
            _ => return Err("недопустимый путь".to_string()),
        }
    }
    Ok(out)
}

/// Нормализовать сегмент пути для release-seeds: убираем разделители пути и
/// запрещённые в Windows символы, чтобы имя ассета лечь как файл, а не путь.
/// Сохраняем пробелы/юникод и точки — они допустимы в именах релизов.
fn sanitize_seed_component(name: &str) -> String {
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
        "unnamed".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Валидация опционального WebSeed mirror-URL: принимаем ТОЛЬКО http(s)://
/// с непустым host. Пустую строку → None (mirror не задан). Всё остальное
/// (ftp://, javascript:, custom-схемы, пробелы, невалидное) → None, silently.
fn sanitize_mirror_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    // Проверяем что после схемы есть host.
    let rest = s
        .splitn(2, "://")
        .nth(1)
        .unwrap_or("")
        .trim_start_matches('/');
    if rest.is_empty() {
        return None;
    }
    // Отсекаем URL с пробелами / \n внутри — они не могут быть магнет-parameter.
    if s.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(s.to_string())
}

/// Percent-encode для URL-строки внутри magnet-parameter `&ws=…`. Оставляем
/// unreserved символы + разделители `:/?#[]@!$&'()*+,;=` — они допустимы
/// внутри URL и не создают конфликта с magnet-парсерами.
fn pct_encode_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' | b'/'
            | b'?' | b'#' | b'[' | b']' | b'@' | b'!' | b'$' | b'\'' | b'(' | b')' | b'*'
            | b'+' | b',' | b';' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}


/// Построить лёгкий манифест дерева (относительный путь + размер) для
/// подписанного анонса. .git и служебные файлы пропускаются.
pub(crate) fn build_tree_manifest(root: &Path) -> Vec<DevTreeEntry> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<DevTreeEntry>) {
        if out.len() >= MAX_TREE {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            if out.len() >= MAX_TREE {
                return;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" || (dir == base && SERVICE_FILES.contains(&name.as_str())) {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            let p = e.path();
            if md.is_dir() {
                walk(&p, base, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(DevTreeEntry {
                    path: rel.to_string_lossy().replace('\\', "/"),
                    size: md.len(),
                });
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Список DevFileEntry каталога `path` из плоского дерева анонса
/// (каталоги синтезируются из префиксов путей).
fn entries_from_tree(tree: &[DevTreeEntry], path: &str) -> Vec<DevFileEntry> {
    let prefix = {
        let p = path.trim_matches('/');
        if p.is_empty() { String::new() } else { format!("{}/", p) }
    };
    let mut dirs: std::collections::BTreeSet<String> = Default::default();
    let mut files: Vec<DevFileEntry> = Vec::new();
    for t in tree {
        let Some(rest) = t.path.strip_prefix(&prefix) else { continue };
        if rest.is_empty() {
            continue;
        }
        match rest.split_once('/') {
            Some((d, _)) => {
                dirs.insert(d.to_string());
            }
            None => files.push(DevFileEntry {
                name: rest.to_string(),
                path: t.path.clone(),
                size: t.size,
                is_dir: false,
            }),
        }
    }
    let mut out: Vec<DevFileEntry> = dirs
        .into_iter()
        .map(|d| DevFileEntry {
            name: d.clone(),
            path: format!("{}{}", prefix, d),
            size: 0,
            is_dir: true,
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(files);
    out
}

// ------------------------------------------------------------
//  Подпись и проверка индекса (legacy bip-schnorr кошелька)
// ------------------------------------------------------------

/// Каноническое сообщение под подпись (схема v2: + starred). Любое изменение
/// repos/starred/seq/updated инвалидирует подпись; злонамеренный сидер не может
/// ни изменить состав, ни накрутить стары, ни выдать старую версию за новую.
fn index_signing_message(
    address: &str,
    seq: u64,
    updated: u64,
    repos_json: &str,
    starred_json: &str,
) -> String {
    format!(
        "devhub:v2:{}:{}:{}:{}:{}",
        address,
        seq,
        updated,
        hex::encode(crate::crypto::sha256(repos_json.as_bytes())),
        hex::encode(crate::crypto::sha256(starred_json.as_bytes()))
    )
}

pub(crate) fn sign_index(index: &mut DevUserIndex, secret: &str) -> Result<(), String> {
    let repos_json = serde_json::to_string(&index.repos).map_err(|e| e.to_string())?;
    let starred_json = serde_json::to_string(&index.starred).map_err(|e| e.to_string())?;
    let msg = index_signing_message(&index.address, index.seq, index.updated, &repos_json, &starred_json);
    let (pubkey, _hash, sig) = crate::sth_node::sign_message(secret, &msg)?;
    index.pubkey = pubkey;
    index.sig = sig;
    Ok(())
}

/// Полная проверка чужого индекса: pubkey → address, подпись, целостность.
pub(crate) fn verify_index(index: &DevUserIndex, expected_address: &str) -> bool {
    if index.address != expected_address {
        return false;
    }
    let pk = match hex::decode(&index.pubkey) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if crate::crypto::address_from_pubkey(&pk) != index.address {
        return false;
    }
    let repos_json = match serde_json::to_string(&index.repos) {
        Ok(j) => j,
        Err(_) => return false,
    };
    let starred_json = match serde_json::to_string(&index.starred) {
        Ok(j) => j,
        Err(_) => return false,
    };
    let msg = index_signing_message(&index.address, index.seq, index.updated, &repos_json, &starred_json);
    let hash = crate::crypto::sha256(msg.as_bytes());
    crate::sth_node::verify_schnorr_legacy(&index.pubkey, &hash, &index.sig)
}

// ------------------------------------------------------------
//  Загрузка / сохранение индексов
// ------------------------------------------------------------

pub(crate) fn my_address() -> Option<String> {
    load_vault_internal().map(|v| v.address)
}

pub(crate) fn load_local_index(app: &tauri::AppHandle, address: &str) -> Option<DevUserIndex> {
    let raw = std::fs::read(index_path(app, address)).ok()?;
    serde_json::from_slice::<DevUserIndex>(&raw).ok()
}

pub(crate) fn save_local_index(app: &tauri::AppHandle, index: &DevUserIndex) -> Result<(), String> {
    let dir = user_dir(app, &index.address);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(index).map_err(|e| e.to_string())?;
    std::fs::write(index_path(app, &index.address), json).map_err(|e| e.to_string())
}

/// Индекс автора по адресу: свой — с диска; чужой — из Sled-кэша (с проверкой
/// подписи и анти-откат гейтом при записи в кэш, см. `store_remote_index`).
pub(crate) fn load_index(app: &tauri::AppHandle, address: &str) -> Option<DevUserIndex> {
    if my_address().as_deref() == Some(address) {
        return load_local_index(app, address);
    }
    let raw = DB
        .get(format!("{}{}", REMOTE_INDEX_PREFIX, address).as_bytes())
        .ok()
        .flatten()?;
    let idx = serde_json::from_slice::<DevUserIndex>(&raw).ok()?;
    if verify_index(&idx, address) {
        Some(idx)
    } else {
        None
    }
}

/// Принять чужой индекс из сети: подпись + монотонность seq (анти-откат).
/// Возвращает Ok(true) если индекс принят (новее виденного), Ok(false) — если
/// отвергнут как устаревший (replay-защита).
pub(crate) fn store_remote_index(index: &DevUserIndex) -> Result<bool, String> {
    if !verify_index(index, &index.address) {
        return Err("подпись индекса не прошла проверку".to_string());
    }
    let key = format!("{}{}", REMOTE_INDEX_PREFIX, index.address);
    let old: Option<DevUserIndex> = DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok());
    if let Some(ref o) = old {
        if index.seq <= o.seq {
            // Анти-откат: старую (или ту же) версию не принимаем.
            return Ok(false);
        }
    }
    let json = serde_json::to_vec(index).map_err(|e| e.to_string())?;
    DB.insert(key.as_bytes(), json).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    // События дня чужого автора: диффим старый и новый индексы, чтобы по клику
    // на день графа было видно, какие репо обновлялись.
    let mut events: Vec<DevEvent> = Vec::new();
    if let Some(o) = old {
        for m in &index.repos {
            match o.repos.iter().find(|r| r.name == m.name) {
                None => events.push(DevEvent { repo: m.name.clone(), action: "created".into(), msg: String::new() }),
                Some(prev) => {
                    if m.releases.len() != prev.releases.len() {
                        events.push(DevEvent { repo: m.name.clone(), action: "release".into(), msg: String::new() });
                    } else if m.updated != prev.updated {
                        events.push(DevEvent { repo: m.name.clone(), action: "push".into(), msg: m.log.first().map(|c| c.message.clone()).unwrap_or_default() });
                    }
                }
            }
        }
        for prev in &o.repos {
            if !index.repos.iter().any(|m| m.name == prev.name) {
                events.push(DevEvent { repo: prev.name.clone(), action: "deleted".into(), msg: String::new() });
            }
        }
    }
    bump_remote_activity(&index.address, &events);
    Ok(true)
}

// ------------------------------------------------------------
//  Граф контрибуций: дневная активность по подписанным seq-обновлениям
// ------------------------------------------------------------

/// unix-секунды → «YYYY-MM-DD» (UTC, алгоритм civil_from_days, без chrono).
fn day_key(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn activity_path(app: &tauri::AppHandle, address: &str) -> PathBuf {
    user_dir(app, address).join("activity.json")
}

type ActivityMap = std::collections::HashMap<String, u32>;
type EventsMap = std::collections::HashMap<String, Vec<DevEvent>>;
/// Кап событий на день (защита от разбухания).
const MAX_EVENTS_PER_DAY: usize = 50;

fn events_path(app: &tauri::AppHandle, address: &str) -> PathBuf {
    user_dir(app, address).join("events.json")
}

/// Записать СВОЁ событие: +1 к счётчику дня (граф) + событие в events.json
/// (список «какие репо обновлялись» по клику на день).
pub(crate) fn record_event(app: &tauri::AppHandle, address: &str, repo: &str, action: &str) {
    record_event_msg(app, address, repo, action, "");
}

/// Как record_event, но с сообщением коммита (события профиля).
pub(crate) fn record_event_msg(
    app: &tauri::AppHandle,
    address: &str,
    repo: &str,
    action: &str,
    msg: &str,
) {
    bump_activity_count(app, address, 1);
    let path = events_path(app, address);
    let mut map: EventsMap = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let day = map.entry(day_key(now_secs())).or_default();
    if day.len() < MAX_EVENTS_PER_DAY {
        day.push(DevEvent {
            repo: repo.to_string(),
            action: action.to_string(),
            msg: msg.chars().take(200).collect(),
        });
    }
    if let Ok(json) = serde_json::to_vec(&map) {
        let _ = std::fs::write(&path, json);
    }
}

/// +n контрибуций СВОЕМУ автору за сегодня (файл на диске, Sled не трогаем).
fn bump_activity_count(app: &tauri::AppHandle, address: &str, n: u32) {
    let path = activity_path(app, address);
    let mut map: ActivityMap = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    *map.entry(day_key(now_secs())).or_insert(0) += n;
    let _ = std::fs::create_dir_all(user_dir(app, address));
    if let Ok(json) = serde_json::to_vec(&map) {
        let _ = std::fs::write(&path, json);
    }
}

/// Контрибуции + события ЧУЖОГО автора (лёгкие JSON в Sled-кэше).
fn bump_remote_activity(address: &str, events: &[DevEvent]) {
    let n = events.len().max(1) as u32;
    let key = format!("{}{}", REMOTE_ACTIVITY_PREFIX, address);
    let mut map: ActivityMap = DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default();
    *map.entry(day_key(now_secs())).or_insert(0) += n;
    if let Ok(json) = serde_json::to_vec(&map) {
        let _ = DB.insert(key.as_bytes(), json);
    }
    if !events.is_empty() {
        let ekey = format!("devhub:events:{}", address);
        let mut emap: EventsMap = DB
            .get(ekey.as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        let day = emap.entry(day_key(now_secs())).or_default();
        for e in events {
            if day.len() >= MAX_EVENTS_PER_DAY {
                break;
            }
            day.push(e.clone());
        }
        if let Ok(json) = serde_json::to_vec(&emap) {
            let _ = DB.insert(ekey.as_bytes(), json);
        }
    }
}

/// Дневная активность автора (карта «YYYY-MM-DD» → контрибуции) для зелёного
/// GitHub-графа. Свой — с диска, чужой — из Sled-кэша.
#[tauri::command]
pub(crate) async fn devhub_activity(
    app: tauri::AppHandle,
    username: String,
) -> Result<ActivityMap, String> {
    let address = resolve_dev_username(&username).ok_or("Имя не найдено в реестре")?;
    if my_address().as_deref() == Some(address.as_str()) {
        return Ok(std::fs::read(activity_path(&app, &address))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default());
    }
    Ok(DB
        .get(format!("{}{}", REMOTE_ACTIVITY_PREFIX, address).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default())
}

/// События конкретного дня («какие репо обновлялись») по клику на клетку графа.
#[tauri::command]
pub(crate) async fn devhub_day_events(
    app: tauri::AppHandle,
    username: String,
    date: String,
) -> Result<Vec<DevEvent>, String> {
    let address = resolve_dev_username(&username).ok_or("Имя не найдено в реестре")?;
    let map: EventsMap = if my_address().as_deref() == Some(address.as_str()) {
        std::fs::read(events_path(&app, &address))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    } else {
        DB.get(format!("devhub:events:{}", address).as_bytes())
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default()
    };
    Ok(map.get(&date).cloned().unwrap_or_default())
}

// ------------------------------------------------------------
//  P2P-синк: публикация своего индекса и загрузка чужих
// ------------------------------------------------------------

/// Опубликовать СВОЙ индекс в сеть: DHT-анонс + сидинг index.json как
/// iroh-blob + регистрация его хэша в биконе (v2-ответ). Вызывается после
/// каждого подписанного изменения (create/delete/push/release).
pub(crate) async fn publish_index(app: &tauri::AppHandle, address: &str) {
    crate::p2p::announce_dev_user(address);
    let path = index_path(app, address);
    if let Some(hash) = crate::p2p::seed_file(app, &path.to_string_lossy()).await {
        crate::p2p::register_dev_index_hash(address, &hash);
    }
    // Мгновенный gossip-анонс: NAT-proof канал через iroh-relay (DHT/бикон
    // за NAT могут быть недостижимы — gossip доносит анонс всегда).
    #[cfg(feature = "p2p")]
    crate::chat::gossip::devhub_announce_now();
}

/// Живые сидеры автора (обновляется при каждом dev_discover-синке).
pub(crate) fn seeders_for(address: &str) -> u32 {
    DB.get(format!("devhub:seeders:{}", address).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Троттлинг синков per-address (защита сети от повторных DHT-запросов).
static SYNC_LAST: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
    std::sync::Mutex::new(None);
const SYNC_MIN_INTERVAL_SECS: u64 = 60;

/// P2P-синк индекса ЧУЖОГО автора: DHT → биконы (v2 несёт хэш индекса) →
/// swarm-загрузка blob'а → проверка подписи + seq (анти-откат) → Sled-кэш.
/// Pull-based и троттлится (1 раз в минуту на автора) — сеть не перегружается.
pub(crate) async fn sync_remote(app: &tauri::AppHandle, username: &str) -> Result<bool, String> {
    let address = resolve_dev_username(username).ok_or("Имя не найдено в реестре")?;
    sync_remote_at(app, username, &address).await
}

/// Как sync_remote, но адрес уже известен (gossip-анонс несёт пару имя+адрес —
/// работает даже когда on-chain реестр имён ещё не проиндексирован).
pub(crate) async fn sync_remote_at(
    app: &tauri::AppHandle,
    username: &str,
    address: &str,
) -> Result<bool, String> {
    let address = address.to_string();
    if my_address().as_deref() == Some(address.as_str()) {
        return Ok(false);
    }
    {
        let mut guard = SYNC_LAST.lock().unwrap();
        let map = guard.get_or_insert_with(Default::default);
        let now = now_secs();
        if let Some(last) = map.get(&address) {
            if now.saturating_sub(*last) < SYNC_MIN_INTERVAL_SECS {
                return Ok(false);
            }
        }
        map.insert(address.clone(), now);
    }
    let (providers, mut xh, seeders) = crate::p2p::devhub_discover(&address).await;
    let _ = DB.insert(
        format!("devhub:seeders:{}", address).as_bytes(),
        seeders.to_string().into_bytes(),
    );
    // Фолбэк: хэш индекса из gossip-анонса (live v2-бикон мог не ответить,
    // но провайдеры из кэша + хэш из анонса дают полноценный синк).
    if xh.is_none() {
        xh = DB
            .get(format!("devhub:gossip:xh:{}", address).as_bytes())
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .filter(|s| !s.is_empty());
    }
    let (Some(hash), false) = (xh, providers.is_empty()) else {
        return Ok(false); // сидеры не найдены либо никто не ответил v2
    };
    let tmp = std::env::temp_dir().join(format!("devidx-{}.json", &hash[..hash.len().min(12)]));
    crate::p2p::fetch_file(app, &providers[0], &hash, &tmp.to_string_lossy(), &providers[1..]).await?;
    let raw = std::fs::read(&tmp).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    let idx: DevUserIndex =
        serde_json::from_slice(&raw).map_err(|_| "повреждённый индекс автора".to_string())?;
    if idx.address != address {
        return Err("индекс не принадлежит этому автору".to_string());
    }
    // Прошлый принятый индекс — для diff'а «что нового» в уведомлении.
    let prev: Option<DevUserIndex> = DB
        .get(format!("{}{}", REMOTE_INDEX_PREFIX, address).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok());
    let accepted = store_remote_index(&idx)?;
    if accepted {
        use tauri::Emitter;
        crate::syslog::log(
            "devhub",
            format!("sync dev://{}: {} repos, seq {}, seeders {}", username, idx.repos.len(), idx.seq, seeders),
        );
        let _ = app.emit("devhub-synced", serde_json::json!({ "username": username }));
        // Тост «автор запушил» для авторов из моих подписок.
        notify_followed_push(app, &idx, prev.as_ref());
    }
    if accepted {
        // Входящие Pull Request'ы автора (если он публиковал prs.json).
        crate::devhub_pr::sync_incoming(app, &idx).await;
    }
    Ok(accepted)
}

/// Мой ли это адрес (для IPC-хэндлеров).
pub(crate) fn is_my_address(address: &str) -> bool {
    my_address().as_deref() == Some(address)
}

/// Уведомление «автор из подписок обновил репозиторий»: эмитим событие с
/// самым свежим изменённым репо — UI показывает тост (не пропустить релиз).
fn notify_followed_push(app: &tauri::AppHandle, idx: &DevUserIndex, prev: Option<&DevUserIndex>) {
    let me = my_address().unwrap_or_default();
    if me.is_empty() {
        return;
    }
    let following = crate::chat::cached_following(&me);
    if !following.iter().any(|a| a == &idx.address) {
        return;
    }
    let mut best: Option<&DevRepoMeta> = None;
    for m in &idx.repos {
        let changed = match prev.and_then(|o| o.repos.iter().find(|r| r.name == m.name)) {
            None => true,
            Some(o) => m.updated > o.updated,
        };
        if changed && best.map(|b| m.updated > b.updated).unwrap_or(true) {
            best = Some(m);
        }
    }
    let Some(m) = best else { return };
    use tauri::Emitter;
    let _ = app.emit(
        "devhub-author-push",
        serde_json::json!({
            "username": idx.username,
            "repo": m.name,
            "message": m.log.first().map(|c| c.message.clone()).unwrap_or_default(),
        }),
    );
}

/// Самолечение легаси-репо: если СВОЙ репозиторий создан до появления
/// генератора git-истории (bundle отсутствует), строим initial commit из
/// рабочих файлов, сидируем и вписываем в подписанный индекс.
async fn ensure_own_bundle(
    app: &tauri::AppHandle,
    address: &str,
    repo: &str,
) -> Result<PathBuf, String> {
    let dir = repo_dir(app, address, repo);
    if !dir.exists() {
        return Err(format!("репозиторий «{}» не найден", repo));
    }
    let username = crate::naming::get_my_username().ok_or("нет on-chain имени")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован — разблокируйте PIN-кодом в клиенте")?;
    let now = now_secs();
    let (bundle, commit_hex) = crate::devhub_git::bundle_from_dir(&dir, &username, "Initial commit", now)?;
    let bpath = bundle_path(app, address, repo);
    std::fs::write(&bpath, &bundle).map_err(|e| e.to_string())?;
    let refs = vec![DevRef { sha: commit_hex, name: "refs/heads/main".to_string() }];
    let refs_json = serde_json::to_vec(&refs).map_err(|e| e.to_string())?;
    std::fs::write(refs_path(app, address, repo), refs_json).map_err(|e| e.to_string())?;
    let bundle_hash = crate::p2p::seed_file(app, &bpath.to_string_lossy())
        .await
        .unwrap_or_default();
    let mut index = load_local_index(app, address).ok_or("Индекс DevHub не найден")?;
    if let Some(meta) = index.repos.iter_mut().find(|m| m.name == repo) {
        meta.bundle_hash = bundle_hash;
        meta.refs = refs.clone();
        if meta.commits == 0 {
            meta.commits = 1;
        }
        if meta.log.is_empty() {
            meta.log = vec![DevCommit {
                sha: refs[0].sha.clone(),
                message: "Initial commit".to_string(),
                author: username.clone(),
                date: now,
            }];
        }
        meta.tree = build_tree_manifest(&dir);
        meta.updated = now;
    }
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(app, &index)?;
    publish_index(app, address).await;
    crate::syslog::log("devhub", format!("initial commit generated for «{}» (legacy repo)", repo));
    Ok(bpath)
}

/// Гарантировать наличие bundle репозитория на диске: свой/кэш — мгновенно,
/// чужой — swarm-загрузка blob'а по bundle_hash из подписанного индекса.
pub(crate) async fn ensure_bundle(
    app: &tauri::AppHandle,
    username: &str,
    repo: &str,
) -> Result<PathBuf, String> {
    let address = resolve_dev_username(username).ok_or("Имя не найдено в реестре")?;
    let path = bundle_path(app, &address, repo);
    if path.exists() {
        return Ok(path);
    }
    if is_my_address(&address) {
        // Легаси-репо без git-истории: генерируем initial commit на лету.
        return ensure_own_bundle(app, &address, repo).await;
    }
    if load_index(app, &address).is_none() {
        sync_remote(app, username).await?;
    }
    let idx = load_index(app, &address).ok_or("индекс автора недоступен (сидеры офлайн?)")?;
    let meta = idx
        .repos
        .iter()
        .find(|m| m.name == repo)
        .ok_or_else(|| format!("репозиторий {}/{} не найден", username, repo))?;
    if meta.bundle_hash.is_empty() {
        return Err("автор ещё не публиковал git-историю (bundle) этого репозитория".to_string());
    }
    // Провайдеры: свежие из discovery (кэшируются в dev_discover).
    let (providers, _, _) = crate::p2p::devhub_discover(&address).await;
    if providers.is_empty() {
        return Err("сидеры репозитория сейчас офлайн".to_string());
    }
    crate::p2p::fetch_file(
        app,
        &providers[0],
        &meta.bundle_hash,
        &path.to_string_lossy(),
        &providers[1..],
    )
    .await?;
    Ok(path)
}

// ------------------------------------------------------------
//  ⭐ Подписанные стары + фоновый ре-синк известных авторов
// ------------------------------------------------------------

/// Честный локальный рейтинг: сколько ИЗВЕСТНЫХ (верифицированных) авторов
/// отметили каждый "username/repo". Каждая звезда подписана ключом кошелька
/// и подкреплена on-chain именем — накрутка требует покупки имён.
pub(crate) fn star_counts(app: &tauri::AppHandle) -> std::collections::HashMap<String, u32> {
    let mut out: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for idx in all_known_indexes(app) {
        for key in &idx.starred {
            *out.entry(key.clone()).or_insert(0) += 1;
        }
    }
    out
}

/// Поставить/снять ⭐: тоггл в подписанном списке starred моего индекса
/// (seq+1 → переподпись → публикация). Возвращает новое состояние.
#[tauri::command]
pub(crate) async fn devhub_toggle_star(
    app: tauri::AppHandle,
    author: String,
    repo: String,
) -> Result<bool, String> {
    let author = crate::dev_router::sanitize_segment("username", &author).map_err(|e| e.to_string())?;
    let repo = crate::dev_router::sanitize_segment("repo", &repo).map_err(|e| e.to_string())?;
    let username = crate::naming::get_my_username()
        .ok_or("Для старов нужно on-chain имя (u://) — оно подписывает вашу звезду")?;
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let key = format!("{}/{}", author, repo);
    let mut index = load_local_index(&app, &address).unwrap_or(DevUserIndex {
        version: 1,
        username: username.clone(),
        address: address.clone(),
        pubkey: String::new(),
        seq: 0,
        updated: 0,
        repos: Vec::new(),
        starred: Vec::new(),
        prs_hash: String::new(),
        merged_prs: Vec::new(),
        sig: String::new(),
    });
    let starred_now = if let Some(pos) = index.starred.iter().position(|k| k == &key) {
        index.starred.remove(pos);
        false
    } else {
        if index.starred.len() >= 500 {
            return Err("Лимит: не более 500 старов на аккаунт".to_string());
        }
        index.starred.push(key.clone());
        true
    };
    index.username = username;
    index.seq += 1;
    index.updated = now_secs();
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    crate::syslog::log(
        "devhub",
        format!("{} dev://{} (seq {})", if starred_now { "⭐ star" } else { "☆ unstar" }, key, index.seq),
    );
    Ok(starred_now)
}

/// Список "username/repo", которые отметил Я (для отрисовки кнопки).
#[tauri::command]
pub(crate) async fn devhub_starred_by_me(app: tauri::AppHandle) -> Vec<String> {
    my_address()
        .and_then(|a| load_local_index(&app, &a))
        .map(|i| i.starred)
        .unwrap_or_default()
}

/// Все известные авторы = ВСЕ зарегистрированные on-chain имена (локальный
/// индекс naming_svc, кап 300) без моего. Реестр имён — это и есть каталог
/// потенциальных DevHub-авторов: у кого индекс не опубликован, синк дёшево
/// закончится «сидеры не найдены».
fn known_authors() -> Vec<String> {
    let mine = crate::naming::get_my_username();
    let mut out = Vec::new();
    for kv in DB.scan_prefix(b"naming_svc:user:").flatten() {
        if let Ok(ks) = std::str::from_utf8(&kv.0) {
            let name = ks.trim_start_matches("naming_svc:user:").to_string();
            if !name.is_empty() && mine.as_deref() != Some(name.as_str()) {
                out.push(name);
            }
        }
        if out.len() >= 300 {
            break;
        }
    }
    // Авторы, известные по gossip-анонсам (реестр имён мог их ещё не видеть).
    for kv in DB.scan_prefix(b"devhub:gossipname:").flatten() {
        if let Ok(ks) = std::str::from_utf8(&kv.0) {
            let name = ks.trim_start_matches("devhub:gossipname:").to_string();
            if !name.is_empty()
                && mine.as_deref() != Some(name.as_str())
                && !out.contains(&name)
            {
                out.push(name);
            }
        }
        if out.len() >= 300 {
            break;
        }
    }
    out
}

/// Дифф коммита (вкладка «Коммиты» → клик по коммиту). Требует git CLI.
#[tauri::command]
pub(crate) async fn devhub_commit_diff(
    app: tauri::AppHandle,
    username: String,
    repo: String,
    sha: String,
) -> Result<String, String> {
    let bundle = ensure_bundle(&app, &username, &repo).await?;
    tokio::task::spawn_blocking(move || crate::devhub_git::diff_from_bundle(&bundle, &sha))
        .await
        .map_err(|e| e.to_string())?
}

/// Немедленный опрос сети: синкаем всех известных on-chain авторов (с разносом,
/// троттлинг 60 с/автор внутри sync_remote). Возвращает число опрашиваемых.
#[tauri::command]
pub(crate) async fn devhub_discover_network(app: tauri::AppHandle) -> u32 {
    let authors = known_authors();
    let n = authors.len() as u32;
    let preview: Vec<&str> = authors.iter().take(10).map(|s| s.as_str()).collect();
    crate::syslog::log(
        "devhub",
        format!(
            "network poll: {} on-chain names{}{}",
            n,
            if n > 0 { ": " } else { " (name registry still empty — wait ~90 s for indexing)" },
            preview.join(", ")
        ),
    );
    std::thread::spawn(move || {
        // Сначала анонсируем себя (served/extra/сид индекса) — взаимная
        // видимость при одновременном нажатии «Обновить» на двух клиентах.
        {
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(addr) = local_devhub_address(&app2) {
                    publish_index(&app2, &addr).await;
                }
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
        for username in authors {
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = sync_remote(&app2, &username).await;
            });
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
    });
    n
}

/// Интервал фонового ре-синка известных авторов.
const RESYNC_INTERVAL_SECS: u64 = 30 * 60;
/// Пауза после старта (даём DHT забутстрапиться).
const RESYNC_START_DELAY_SECS: u64 = 25;

/// Фоновый цикл живого поиска: раз в 30 минут ре-синкаем всех известных
/// авторов (индексы копеечные — килобайты; троттлинг внутри sync_remote
/// дополнительно защищает сеть). Синки разнесены по 3 с, не залпом.
pub(crate) fn spawn_resync(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(RESYNC_START_DELAY_SECS));
        // При старте публикуем СВОЙ индекс: заполняет served/extra бикона и
        // сидирует index.json — иначе LAN/DHT-probe соседей остаются без
        // ответа до первого локального изменения.
        {
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(addr) = local_devhub_address(&app2) {
                    publish_index(&app2, &addr).await;
                    crate::syslog::log("devhub", "initial announcement of own index published".to_string());
                }
            });
        }
        loop {
            // offline: ре-синк бессмысленен — короткая пауза до восстановления сети
            if !crate::netwatch::is_online() {
                std::thread::sleep(std::time::Duration::from_secs(30));
                continue;
            }
            let authors = known_authors();
            if !authors.is_empty() {
                crate::syslog::log("devhub", format!("background re-sync of {} authors…", authors.len()));
            }
            for username in authors {
                let app2 = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = sync_remote(&app2, &username).await;
                });
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            std::thread::sleep(std::time::Duration::from_secs(RESYNC_INTERVAL_SECS));
        }
    });
}

// ------------------------------------------------------------
//  Git-хранилище репозитория: bundle + refs (для git-remote-dev)
// ------------------------------------------------------------

pub(crate) fn bundle_path(app: &tauri::AppHandle, address: &str, repo: &str) -> PathBuf {
    repo_dir(app, address, repo).join("repo.bundle")
}

pub(crate) fn refs_path(app: &tauri::AppHandle, address: &str, repo: &str) -> PathBuf {
    repo_dir(app, address, repo).join("refs.json")
}

/// Текущие git-рефы репозитория (пусто, если push ещё не было).
pub(crate) fn repo_refs(app: &tauri::AppHandle, address: &str, repo: &str) -> Vec<DevRef> {
    std::fs::read(refs_path(app, address, repo))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Принять `git push` от git-remote-dev (через loopback-IPC): сохранить полный
/// bundle + refs, засидировать bundle как iroh-blob (bundle_hash в подписанном
/// индексе → P2P-клонирование), обновить метаданные, seq+1, переподписать,
/// опубликовать и засчитать контрибуцию. Репозиторий создаётся автоматически,
/// если push идёт в ещё не существующее имя (как на GitHub через git push).
pub(crate) async fn apply_push(
    app: &tauri::AppHandle,
    username: &str,
    repo: &str,
    bundle: &[u8],
    refs: Vec<DevRef>,
    commits: u32,
) -> Result<(), String> {
    let repo = crate::dev_router::sanitize_segment("repo", repo).map_err(|e| e.to_string())?;
    let my_name = crate::naming::get_my_username()
        .ok_or("Сначала зарегистрируйте имя пользователя (u://)")?;
    if my_name != username {
        return Err(format!("push разрешён только в свои репозитории (@{})", my_name));
    }
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован — разблокируйте PIN-кодом в клиенте")?;

    let dir = repo_dir(app, &address, &repo);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bpath = bundle_path(app, &address, &repo);
    std::fs::write(&bpath, bundle).map_err(|e| e.to_string())?;
    let refs_json = serde_json::to_vec(&refs).map_err(|e| e.to_string())?;
    std::fs::write(refs_path(app, &address, &repo), refs_json).map_err(|e| e.to_string())?;
    // Извлекаем рабочие файлы из bundle — вкладка «Код» показывает свежие
    // файлы сразу после git push. Заодно забираем историю коммитов (git log).
    // head_sha обязателен: push-bundle не содержит HEAD, без явного checkout
    // рабочая копия остаётся пустой (git clone молча выходит с кодом 0).
    let head_sha = refs
        .iter()
        .find(|r| r.name == "refs/heads/main" || r.name == "refs/heads/master")
        .or_else(|| refs.iter().find(|r| r.name.starts_with("refs/heads/")))
        .or_else(|| refs.first())
        .map(|r| r.sha.clone())
        .unwrap_or_default();
    let mut push_log: Vec<DevCommit> = Vec::new();
    {
        let dir2 = dir.clone();
        let bpath2 = bpath.clone();
        let sha2 = head_sha.clone();
        match tokio::task::spawn_blocking(move || {
            crate::devhub_git::checkout_bundle(&dir2, &bpath2, &sha2)
        })
        .await
        {
            Ok(Ok(log)) => push_log = log,
            Ok(Err(e)) => crate::syslog::log(
                "devhub",
                format!("checkout after push failed (git CLI): {}", e),
            ),
            Err(e) => crate::syslog::log("devhub", format!("checkout task: {e}")),
        }
    }
    // Сидируем bundle как iroh-blob: его хэш попадёт в подписанный индекс,
    // и любой пир сможет клонировать репозиторий по P2P.
    let bundle_hash = crate::p2p::seed_file(app, &bpath.to_string_lossy())
        .await
        .unwrap_or_default();
    // Лёгкое дерево файлов — едет в подписанном индексе: чужие клиенты
    // показывают вкладку «Код» без скачивания bundle.
    let tree = build_tree_manifest(&dir);

    let branches: Vec<String> = refs
        .iter()
        .filter_map(|r| r.name.strip_prefix("refs/heads/").map(|s| s.to_string()))
        .collect();
    let default_branch = if branches.iter().any(|b| b == "main") {
        "main".to_string()
    } else {
        branches.first().cloned().unwrap_or_else(|| "main".to_string())
    };

    let now = now_secs();
    let mut index = load_local_index(app, &address).unwrap_or(DevUserIndex {
        version: 1,
        username: my_name.clone(),
        address: address.clone(),
        pubkey: String::new(),
        seq: 0,
        updated: 0,
        repos: Vec::new(),
        starred: Vec::new(),
        prs_hash: String::new(),
        merged_prs: Vec::new(),
        sig: String::new(),
    });
    let mut created_new = false;
    match index.repos.iter_mut().find(|m| m.name == repo) {
        Some(meta) => {
            meta.updated = now;
            meta.commits = commits;
            meta.bundle_hash = bundle_hash;
            meta.refs = refs;
            meta.tree = tree.clone();
            if !push_log.is_empty() {
                meta.log = push_log.clone();
            }
            if !branches.is_empty() {
                meta.branches = branches;
                meta.default_branch = default_branch;
            }
        }
        None => {
            created_new = true;
            index.repos.push(DevRepoMeta {
                name: repo.clone(),
                description: String::new(),
                license: String::new(),
                language: String::new(),
                default_branch,
                branches: if branches.is_empty() { vec!["main".into()] } else { branches },
                created: now,
                updated: now,
                commits,
                releases: Vec::new(),
                bundle_hash,
                refs,
                log: push_log.clone(),
                tree,
                forked_from: String::new(),
            });
        }
    }
    index.username = my_name;
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(app, &index)?;
    publish_index(app, &address).await;
    record_event_msg(
        app,
        &address,
        &repo,
        if created_new { "created" } else { "push" },
        push_log.first().map(|c| c.message.as_str()).unwrap_or(""),
    );
    crate::syslog::log(
        "devhub",
        format!("git push accepted: dev://{}/{} ({} commits, seq {})", index.username, repo, commits, index.seq),
    );
    Ok(())
}

/// Создать релиз (аналог GitHub Releases): тег + заметки + ассеты из f://.
/// Ассеты живут в файлообменнике и скачиваются ТОЛЬКО по явному клику.
#[tauri::command]
pub(crate) async fn devhub_create_release(
    app: tauri::AppHandle,
    repo: String,
    tag: String,
    title: String,
    notes: String,
    assets: Vec<DevAsset>,
) -> Result<DevRelease, String> {
    let tag = tag.trim().to_string();
    if tag.is_empty() || tag.len() > 64 || !tag.chars().all(|c| c.is_ascii_alphanumeric() || ".-_+".contains(c)) {
        return Err("Некорректный тег (латиница, цифры и . - _ +, до 64 символов)".to_string());
    }
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let mut index = load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    let now = now_secs();

    // v1.62 TORRENT-транспорт релизов: для каждого ассета берём его файл из
    // файлообменника (уже засидированный blake3-blob), автоматически пакуем
    // в torrent через существующий `torrent::create_torrent_from_path`,
    // регистрируем в librqbit на раздачу и получаем magnet-URI. Магнет
    // кладётся прямо в подписанный DevAsset — целостность гарантируется
    // подписью автора над всем IndexRecord (`sign_index`), подделать
    // невозможно без приватного ключа. Для старых ассетов без файла в
    // хранилище магнет остаётся пустым (backward-compat).
    //
    // ВАЖНО: файлы в filesdata лежат под именем `<blake3-cid>` (для дедупа).
    // Если торрентовать напрямую этот путь — в .torrent попадёт CID-хэш
    // вместо реального имени (напр. `MyApp-v1.exe`), и удалённые клиенты
    // сохранят у себя файл с непонятным именем. Поэтому сначала создаём
    // hardlink (или копию — fallback для FAT/сетевых FS) в
    // `data/release-seeds/<repo>/<tag>/<original_name>` и торрентуем ЕГО.
    // Ссылка занимает 0 байт диска, но даёт нам корректное имя в торренте.
    let seeds_root = crate::ensure_data_dir(&app)
        .join("release-seeds")
        .join(sanitize_seed_component(&repo))
        .join(sanitize_seed_component(&tag));
    let _ = std::fs::create_dir_all(&seeds_root);

    let mut enriched_assets: Vec<DevAsset> = Vec::with_capacity(assets.len());
    for a in assets {
        let mut asset = a;
        if asset.magnet.is_empty() && !asset.cid.is_empty() {
            let src = crate::files::file_store_path(&app, &asset.cid);
            if src.exists() {
                // Готовим seed-путь с оригинальным именем ассета.
                let safe_name = sanitize_seed_component(&asset.name);
                let seed_path = seeds_root.join(&safe_name);
                // Идемпотентно: удаляем старый хардлинк/файл, если есть.
                let _ = std::fs::remove_file(&seed_path);
                // 1) hardlink — мгновенно, 0 байт. 2) copy — fallback.
                let prepared = if std::fs::hard_link(&src, &seed_path).is_ok() {
                    true
                } else {
                    match std::fs::copy(&src, &seed_path) {
                        Ok(_) => true,
                        Err(e) => {
                            crate::syslog::log(
                                "devhub",
                                format!(
                                    "release-seed prepare FAIL for {}: {}",
                                    asset.name, e
                                ),
                            );
                            false
                        }
                    }
                };
                if prepared {
                    match crate::torrent::create_torrent_from_path(
                        app.clone(),
                        seed_path.to_string_lossy().to_string(),
                        None,
                        Vec::new(),
                        Some(format!("dev://{}/{} · {}", address, repo, tag)),
                        Some(false),
                        None,
                    )
                    .await
                    {
                        Ok(meta) => {
                            let mut mag = crate::torrent::magnet_from_info_hash(&meta.info_hash);
                            // v1.63.2 Publisher WebSeed: если автор указал
                            // mirror-URL — приписываем `&ws=<pct>` к magnet
                            // (BEP-19). librqbit подхватит его как fallback,
                            // когда в swarm нет пиров. Некорректные URL
                            // отсеивает `sanitize_mirror_url`.
                            if let Some(mirror) = sanitize_mirror_url(&asset.mirror) {
                                mag.push_str("&ws=");
                                mag.push_str(&pct_encode_url(&mirror));
                            }
                            asset.magnet = mag;
                            crate::syslog::log(
                                "devhub",
                                format!(
                                    "release-torrent: {} → {} (infohash {}, mirror={})",
                                    asset.name,
                                    asset.magnet,
                                    meta.info_hash,
                                    asset.mirror.is_empty()
                                ),
                            );
                        }
                        Err(e) => {
                            crate::syslog::log(
                                "devhub",
                                format!("release-torrent FAIL for {}: {}", asset.name, e),
                            );
                        }
                    }
                }
            } else {
                crate::syslog::log(
                    "devhub",
                    format!("release-torrent SKIP {}: file not found on disk", asset.name),
                );
            }
        }
        enriched_assets.push(asset);
    }

    let release = DevRelease {
        tag: tag.clone(),
        title: title.trim().to_string(),
        notes: notes.trim().to_string(),
        created: now,
        assets: enriched_assets,
    };
    {
        let meta = index
            .repos
            .iter_mut()
            .find(|m| m.name == repo)
            .ok_or_else(|| format!("Репозиторий «{}» не найден", repo))?;
        if meta.releases.iter().any(|r| r.tag == tag) {
            return Err(format!("Релиз с тегом «{}» уже существует", tag));
        }
        meta.releases.insert(0, release.clone());
        meta.updated = now;
    }
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event(&app, &address, &repo, "release");
    crate::syslog::log("devhub", format!("release {} {} published (seq {})", repo, tag, index.seq));
    Ok(release)
}

/// Редактировать метаданные существующего релиза (title/notes). Тег и ассеты
/// не меняем — тег служит первичным ключом, а ассеты завязаны на magnet-URI
/// и раздачу; их полная замена = удаление+создание релиза. Пересобирает
/// подписанный индекс (`seq+1`), чтобы удалённые клиенты увидели правки.
#[tauri::command]
pub(crate) async fn devhub_edit_release(
    app: tauri::AppHandle,
    repo: String,
    tag: String,
    title: String,
    notes: String,
) -> Result<DevRelease, String> {
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let mut index = load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    let now = now_secs();
    let updated_release = {
        let meta = index
            .repos
            .iter_mut()
            .find(|m| m.name == repo)
            .ok_or_else(|| format!("Репозиторий «{}» не найден", repo))?;
        let rel = meta
            .releases
            .iter_mut()
            .find(|r| r.tag == tag)
            .ok_or_else(|| format!("Релиз с тегом «{}» не найден", tag))?;
        rel.title = title.trim().to_string();
        rel.notes = notes.trim().to_string();
        meta.updated = now;
        rel.clone()
    };
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event(&app, &address, &repo, "release-edit");
    crate::syslog::log(
        "devhub",
        format!("release {} {} edited (seq {})", repo, tag, index.seq),
    );
    Ok(updated_release)
}

/// Удалить релиз из репозитория. Метаданные исчезают из подписанного индекса
/// (seq+1), а связанные с ассетами торренты продолжают жить в клиенте — их
/// пользователь может остановить/удалить вручную в `sn://torrents`. Файлы в
/// файлохранилище (`data/filesdata/<xx>/<cid>`) сохраняются, поскольку тот
/// же CID может быть переиспользован в другом релизе / другими пирами.
#[tauri::command]
pub(crate) async fn devhub_delete_release(
    app: tauri::AppHandle,
    repo: String,
    tag: String,
) -> Result<(), String> {
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let mut index = load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    let now = now_secs();
    let removed = {
        let meta = index
            .repos
            .iter_mut()
            .find(|m| m.name == repo)
            .ok_or_else(|| format!("Репозиторий «{}» не найден", repo))?;
        let before = meta.releases.len();
        meta.releases.retain(|r| r.tag != tag);
        if meta.releases.len() == before {
            return Err(format!("Релиз с тегом «{}» не найден", tag));
        }
        meta.updated = now;
        before - meta.releases.len()
    };
    if removed == 0 {
        return Err(format!("Релиз с тегом «{}» не найден", tag));
    }
    // Best-effort: удалить staging-папку release-seeds/<repo>/<tag>/ — она
    // содержит только hardlink-и на файлы файлохранилища. Основные файлы
    // остаются нетронутыми.
    let seed_dir = crate::ensure_data_dir(&app)
        .join("release-seeds")
        .join(sanitize_seed_component(&repo))
        .join(sanitize_seed_component(&tag));
    let _ = std::fs::remove_dir_all(&seed_dir);
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event(&app, &address, &repo, "release-delete");
    crate::syslog::log(
        "devhub",
        format!("release {} {} deleted (seq {})", repo, tag, index.seq),
    );
    Ok(())
}

/// Адрес локального DevHub-автора, если кошелёк открыт и индекс существует.
/// Используется циклом сидинга p2p для анонса dev-infohash в DHT.
pub(crate) fn local_devhub_address(app: &tauri::AppHandle) -> Option<String> {
    let addr = my_address()?;
    if index_path(app, &addr).exists() {
        Some(addr)
    } else {
        None
    }
}

/// Все известные локально индексы: свой + верифицированные из Sled-кэша.
// ------------------------------------------------------------
//  Лента обновлений DevHub (свежие пуши авторов сети/подписок)
// ------------------------------------------------------------

/// Элемент ленты: последнее изменение репозитория известного автора.
#[derive(serde::Serialize)]
pub(crate) struct DevFeedItem {
    pub username: String,
    pub address: String,
    pub repo: String,
    /// сообщение последнего коммита (пусто, если лога нет)
    pub message: String,
    pub updated: u64,
    /// автор в моих подписках
    pub followed: bool,
    /// последний тегированный релиз репозитория (если есть) — для бейджа и
    /// прямых ссылок на скачивание в UI ленты.
    #[serde(rename = "latestRelease", skip_serializing_if = "Option::is_none")]
    pub latest_release: Option<DevRelease>,
}

/// Лента свежих пушей: все известные (синкнутые) индексы, кроме моего,
/// отсортированные по времени обновления репо. UI фильтрует по `followed`.
/// async + spawn_blocking: лента читает ВСЕ известные индексы (Sled scan +
/// диск) — на main-потоке это периодически подвешивало dev:// страницы.
#[tauri::command]
pub(crate) async fn devhub_feed(app: tauri::AppHandle) -> Vec<DevFeedItem> {
    tokio::task::spawn_blocking(move || devhub_feed_blocking(&app))
        .await
        .unwrap_or_default()
}

fn devhub_feed_blocking(app: &tauri::AppHandle) -> Vec<DevFeedItem> {
    let me = my_address().unwrap_or_default();
    let following = if me.is_empty() {
        Vec::new()
    } else {
        crate::chat::cached_following(&me)
    };
    let mut out = Vec::new();
    for idx in all_known_indexes(&app) {
        if idx.address == me {
            continue;
        }
        let followed = following.iter().any(|a| a == &idx.address);
        for m in &idx.repos {
            let latest_release = m
                .releases
                .iter()
                .max_by_key(|r| r.created)
                .cloned();
            out.push(DevFeedItem {
                username: idx.username.clone(),
                address: idx.address.clone(),
                repo: m.name.clone(),
                message: m.log.first().map(|c| c.message.clone()).unwrap_or_default(),
                updated: m.updated,
                followed,
                latest_release,
            });
        }
    }
    out.sort_by(|a, b| b.updated.cmp(&a.updated));
    out.truncate(100);
    out
}

pub(crate) fn all_known_indexes(app: &tauri::AppHandle) -> Vec<DevUserIndex> {
    let mut out = Vec::new();
    if let Some(addr) = my_address() {
        if let Some(idx) = load_local_index(app, &addr) {
            out.push(idx);
        }
    }
    for kv in DB.scan_prefix(REMOTE_INDEX_PREFIX.as_bytes()) {
        if let Ok((_, v)) = kv {
            if let Ok(idx) = serde_json::from_slice::<DevUserIndex>(&v) {
                if verify_index(&idx, &idx.address.clone()) {
                    out.push(idx);
                }
            }
        }
    }
    out
}

// ------------------------------------------------------------
//  Шаблоны лицензий (компактные; полный текст — MIT/Unlicense)
// ------------------------------------------------------------

fn license_text(id: &str, owner: &str) -> Option<String> {
    let year = 1970 + (now_secs() / 31_557_600); // приблизительный текущий год
    match id {
        "MIT" => Some(format!(
            "MIT License\n\nCopyright (c) {year} {owner}\n\nPermission is hereby granted, free of charge, to any person obtaining a copy\nof this software and associated documentation files (the \"Software\"), to deal\nin the Software without restriction, including without limitation the rights\nto use, copy, modify, merge, publish, distribute, sublicense, and/or sell\ncopies of the Software, and to permit persons to whom the Software is\nfurnished to do so, subject to the following conditions:\n\nThe above copyright notice and this permission notice shall be included in all\ncopies or substantial portions of the Software.\n\nTHE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\nIMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,\nFITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE\nAUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER\nLIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,\nOUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE\nSOFTWARE.\n"
        )),
        "Apache-2.0" => Some(format!(
            "Copyright {year} {owner}\n\nLicensed under the Apache License, Version 2.0 (the \"License\");\nyou may not use this file except in compliance with the License.\nYou may obtain a copy of the License at\n\n    http://www.apache.org/licenses/LICENSE-2.0\n\nUnless required by applicable law or agreed to in writing, software\ndistributed under the License is distributed on an \"AS IS\" BASIS,\nWITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.\nSee the License for the specific language governing permissions and\nlimitations under the License.\n"
        )),
        "GPL-3.0" => Some(format!(
            "Copyright (C) {year} {owner}\n\nThis program is free software: you can redistribute it and/or modify\nit under the terms of the GNU General Public License as published by\nthe Free Software Foundation, either version 3 of the License, or\n(at your option) any later version.\n\nThis program is distributed in the hope that it will be useful,\nbut WITHOUT ANY WARRANTY; without even the implied warranty of\nMERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the\nGNU General Public License for more details.\n\nYou should have received a copy of the GNU General Public License\nalong with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
        )),
        "BSD-3-Clause" => Some(format!(
            "BSD 3-Clause License\n\nCopyright (c) {year}, {owner}\n\nRedistribution and use in source and binary forms, with or without\nmodification, are permitted provided that the conditions of the\nBSD 3-Clause License are met (see https://opensource.org/license/bsd-3-clause).\n\nTHIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS \"AS IS\" WITHOUT WARRANTY\nOF ANY KIND.\n"
        )),
        "Unlicense" => Some(
            "This is free and unencumbered software released into the public domain.\n\nAnyone is free to copy, modify, publish, use, compile, sell, or\ndistribute this software, either in source code form or as a compiled\nbinary, for any purpose, commercial or non-commercial, and by any\nmeans.\n\nFor more information, please refer to <https://unlicense.org>\n"
                .to_string(),
        ),
        _ => None,
    }
}

// ------------------------------------------------------------
//  Gossip-анонсы DevHub (NAT-proof обнаружение авторов)
// ------------------------------------------------------------

/// Компактный анонс автора в системном gossip-топике (см. chat/gossip.rs).
#[derive(serde::Serialize, serde::Deserialize)]
struct DevGossipAnnounce {
    v: u32,
    /// username автора (on-chain имя)
    u: String,
    /// wallet-адрес (он же dev://-адрес)
    a: String,
    /// iroh EndpointId автора — провайдер blob'ов индекса/bundle'ов
    e: String,
    /// blake3-хэш blob'а подписанного index.json
    x: String,
    /// seq индекса (анти-откат гарантирует store_remote_index)
    s: u64,
    t: u64,
}

/// Собрать СВОЙ анонс (None — нет кошелька/индекса/P2P-ноды).
pub(crate) async fn gossip_announce_payload(app: &tauri::AppHandle) -> Option<Vec<u8>> {
    let address = local_devhub_address(app)?;
    let idx = load_local_index(app, &address)?;
    let e = crate::p2p::node_id(app).await.ok()?;
    let x = crate::p2p::dev_index_hash_for(&address).unwrap_or_default();
    serde_json::to_vec(&DevGossipAnnounce {
        v: 1,
        u: idx.username,
        a: address,
        e,
        x,
        s: idx.seq,
        t: now_secs(),
    })
    .ok()
}

/// Rate-limit входящих анонсов (per-address).
static GOSSIP_SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
    std::sync::Mutex::new(None);

/// Обработать входящий gossip-анонс: закэшировать провайдера + хэш индекса,
/// при новом seq — фоновый синк. Безопасность: анонс лишь подсказка — сам
/// индекс проходит проверку подписи кошелька в store_remote_index, поэтому
/// поддельный анонс даёт максимум мусорный blob, который будет отвергнут.
pub(crate) fn handle_gossip_announce(bytes: &[u8]) {
    if bytes.len() > 1024 {
        return;
    }
    // Та же шина несёт анонсы keyfile'ов платных альбомов + «новый альбом»
    // (другие JSON-форматы). Для эмита `music-new-album` нужен AppHandle.
    #[cfg(feature = "p2p")]
    if let Some(app) = crate::chat::gossip::app_handle() {
        if crate::social::handle_album_gossip(&app, bytes) {
            return;
        }
    }
    let Ok(an) = serde_json::from_slice::<DevGossipAnnounce>(bytes) else {
        return;
    };
    if an.v != 1 || an.u.is_empty() || an.a.is_empty() || an.e.is_empty() {
        return;
    }
    if an.u.len() > 64 || an.a.len() > 64 || an.e.len() > 80 || an.x.len() > 80 {
        return;
    }
    if my_address().as_deref() == Some(an.a.as_str()) {
        return;
    }
    // Анти-спуфинг: если on-chain реестр знает имя — адрес обязан совпадать.
    match crate::naming::resolve_username(an.u.clone()) {
        Some(chain) if chain != an.a => return,
        Some(_) => {}
        None => {
            // Реестр ещё не проиндексировал имя — временный fallback-резолв.
            let _ = DB.insert(
                format!("devhub:gossipname:{}", an.u).as_bytes(),
                an.a.clone().into_bytes(),
            );
        }
    }
    {
        let mut guard = GOSSIP_SEEN.lock().unwrap();
        let map = guard.get_or_insert_with(Default::default);
        let now = now_secs();
        if let Some(last) = map.get(&an.a) {
            if now.saturating_sub(*last) < 30 {
                return;
            }
        }
        map.insert(an.a.clone(), now);
        if map.len() > 2048 {
            map.retain(|_, t| now.saturating_sub(*t) < 3600);
        }
    }
    // EndpointId автора → кэш discovery: dev_discover фолбэкается на него,
    // когда DHT/LAN молчат (blob качается напрямую через iroh, NAT пробивает relay).
    let pkey = format!("dht:providers:dev:{}", an.a);
    let mut ids: Vec<String> = DB
        .get(pkey.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !ids.contains(&an.e) {
        ids.insert(0, an.e.clone());
        ids.truncate(8);
        let _ = DB.insert(pkey.as_bytes(), ids.join(",").into_bytes());
    }
    if !an.x.is_empty() {
        let _ = DB.insert(
            format!("devhub:gossip:xh:{}", an.a).as_bytes(),
            an.x.clone().into_bytes(),
        );
    }
    // Синкаем только если индекс новее уже виденного (иначе анонс — просто keepalive).
    let known_seq = DB
        .get(format!("{}{}", REMOTE_INDEX_PREFIX, an.a).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<DevUserIndex>(&v).ok())
        .map(|i| i.seq)
        .unwrap_or(0);
    if an.s > known_seq {
        crate::syslog::log(
            "devhub",
            format!("gossip announce dev://{} (seq {}) — syncing index", an.u, an.s),
        );
        #[cfg(feature = "p2p")]
        if let Some(app) = crate::chat::gossip::app_handle() {
            let (u, a) = (an.u.clone(), an.a.clone());
            tauri::async_runtime::spawn(async move {
                let _ = sync_remote_at(&app, &u, &a).await;
            });
        }
    }
}

// ------------------------------------------------------------
//  Разрешение имени → адрес (свой шорткат + on-chain реестр)
// ------------------------------------------------------------

/// username → address. Сначала свой (мгновенно, работает даже до индексации
/// свежекупленного имени), затем on-chain реестр naming.rs.
pub(crate) fn resolve_dev_username(username: &str) -> Option<String> {
    if crate::naming::get_my_username().as_deref() == Some(username) {
        return my_address();
    }
    if let Some(a) = crate::naming::resolve_username(username.to_string()) {
        return Some(a);
    }
    // Фолбэк: адрес из gossip-анонса (on-chain реестр имён мог ещё не
    // проиндексироваться — не блокируем обнаружение автора).
    DB.get(format!("devhub:gossipname:{}", username).as_bytes())
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .filter(|s| !s.is_empty())
}

// ------------------------------------------------------------
//  Tauri-команды
// ------------------------------------------------------------

/// Создать новый пустой репозиторий (аналог GitHub «Create a new repository»):
/// папка на диске + README.md + LICENSE.md, обновление и подпись индекса,
/// мгновенный анонс в DHT. Полностью бесплатно — никаких транзакций.
#[tauri::command]
pub(crate) async fn devhub_create_repo(
    app: tauri::AppHandle,
    name: String,
    description: String,
    license: String,
    add_readme: bool,
) -> Result<DevRepoMeta, String> {
    let name = crate::dev_router::sanitize_segment("repo", &name)
        .map_err(|e| e.to_string())?;
    let username = crate::naming::get_my_username()
        .ok_or("Сначала зарегистрируйте имя пользователя (u://) — оно и есть ваш dev://-адрес")?;
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован — разблокируйте PIN-кодом")?;

    let dir = repo_dir(&app, &address, &name);
    if dir.exists() {
        return Err(format!("Репозиторий «{}» уже существует", name));
    }
    let mut index = load_local_index(&app, &address).unwrap_or(DevUserIndex {
        version: 1,
        username: username.clone(),
        address: address.clone(),
        pubkey: String::new(),
        seq: 0,
        updated: 0,
        repos: Vec::new(),
        starred: Vec::new(),
        prs_hash: String::new(),
        merged_prs: Vec::new(),
        sig: String::new(),
    });
    if index.repos.iter().any(|r| r.name == name) {
        return Err(format!("Репозиторий «{}» уже есть в индексе", name));
    }

    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if add_readme {
        let readme = if description.trim().is_empty() {
            format!("# {}\n", name)
        } else {
            format!("# {}\n\n{}\n", name, description.trim())
        };
        std::fs::write(dir.join("README.md"), readme).map_err(|e| e.to_string())?;
    }
    if let Some(text) = license_text(&license, &username) {
        std::fs::write(dir.join("LICENSE.md"), text).map_err(|e| e.to_string())?;
    }

    // Настоящий initial commit (как на GitHub): blob/tree/commit + bundle на
    // чистом Rust — `git clone dev://…` сразу отдаёт README и LICENSE.
    let now = now_secs();
    let mut bundle_hash = String::new();
    let mut refs: Vec<DevRef> = Vec::new();
    let mut commits = 0u32;
    if let Ok((bundle, commit_hex)) = crate::devhub_git::bundle_from_dir(&dir, &username, "Initial commit", now) {
        let bpath = bundle_path(&app, &address, &name);
        std::fs::write(&bpath, &bundle).map_err(|e| e.to_string())?;
        refs = vec![DevRef { sha: commit_hex, name: "refs/heads/main".to_string() }];
        let refs_json = serde_json::to_vec(&refs).map_err(|e| e.to_string())?;
        std::fs::write(refs_path(&app, &address, &name), refs_json).map_err(|e| e.to_string())?;
        bundle_hash = crate::p2p::seed_file(&app, &bpath.to_string_lossy())
            .await
            .unwrap_or_default();
        commits = 1;
    }

    let meta = DevRepoMeta {
        name: name.clone(),
        description: description.trim().to_string(),
        license: if license_text(&license, &username).is_some() { license } else { String::new() },
        language: String::new(),
        default_branch: "main".to_string(),
        branches: vec!["main".to_string()],
        created: now,
        updated: now,
        commits,
        releases: Vec::new(),
        bundle_hash,
        log: refs
            .first()
            .map(|r| {
                vec![DevCommit {
                    sha: r.sha.clone(),
                    message: "Initial commit".to_string(),
                    author: username.clone(),
                    date: now,
                }]
            })
            .unwrap_or_default(),
        refs,
        tree: build_tree_manifest(&dir),
        forked_from: String::new(),
    };
    index.username = username;
    index.repos.push(meta.clone());
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;

    // Публикация в сеть: DHT-анонс + сидинг индекса + хэш в биконе.
    publish_index(&app, &address).await;
    record_event(&app, &address, &name, "created");
    crate::syslog::log(
        "devhub",
        format!("repository created dev://{}/{} (seq {})", index.username, meta.name, index.seq),
    );
    Ok(meta)
}

/// Удалить свой репозиторий: папка с диска + запись из подписанного индекса.
#[tauri::command]
pub(crate) async fn devhub_delete_repo(app: tauri::AppHandle, name: String) -> Result<(), String> {
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let mut index = load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    let before = index.repos.len();
    index.repos.retain(|r| r.name != name);
    if index.repos.len() == before {
        return Err(format!("Репозиторий «{}» не найден", name));
    }
    let _ = std::fs::remove_dir_all(repo_dir(&app, &address, &name));
    index.seq += 1;
    index.updated = now_secs();
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event(&app, &address, &name, "deleted");
    crate::syslog::log("devhub", format!("repository «{}» deleted (seq {})", name, index.seq));
    Ok(())
}

/// Список файлов репозитория (вкладка «Код»): локальная копия с диска, а для
/// чужих репо без неё — лёгкое дерево из подписанного анонса (без скачивания).
#[tauri::command]
pub(crate) async fn devhub_list_files(
    app: tauri::AppHandle,
    username: String,
    repo: String,
    path: String,
) -> Result<Vec<DevFileEntry>, String> {
    let address = resolve_dev_username(&username).ok_or("Имя не найдено в реестре")?;
    let root = repo_dir(&app, &address, &repo);
    if !root.exists() {
        // Чужой репозиторий без локальной копии: список файлов из лёгкого
        // дерева в подписанном анонсе автора — ничего не скачиваем.
        if !is_my_address(&address) {
            if let Some(meta) = load_index(&app, &address)
                .and_then(|idx| idx.repos.into_iter().find(|m| m.name == repo))
            {
                if !meta.tree.is_empty() {
                    return Ok(entries_from_tree(&meta.tree, &path));
                }
            }
        }
        return Err("Автор ещё не опубликовал дерево файлов в анонсе — скачайте файлы от сидеров кнопкой ниже".to_string());
    }
    // Самолечение: если рабочие файлы потерялись (баг пустого checkout из
    // push-bundle без HEAD, ≤1.55.44) — извлекаем их из бандла заново.
    if path.is_empty() {
        let only_service = std::fs::read_dir(&root)
            .map(|rd| {
                rd.flatten().all(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n == ".git" || SERVICE_FILES.contains(&n.as_str())
                })
            })
            .unwrap_or(false);
        let bpath = bundle_path(&app, &address, &repo);
        if only_service && bpath.exists() {
            let head_sha = repo_refs(&app, &address, &repo)
                .first()
                .map(|r| r.sha.clone())
                .unwrap_or_default();
            let root2 = root.clone();
            if let Ok(Ok(_)) = tokio::task::spawn_blocking(move || {
                crate::devhub_git::checkout_bundle(&root2, &bpath, &head_sha)
            })
            .await
            {
                crate::syslog::log(
                    "devhub",
                    format!("{}/{}: working files restored from bundle", username, repo),
                );
            }
        }
    }
    let dir = safe_join(&root, &path)?;
    let rd = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    let mut out: Vec<DevFileEntry> = Vec::new();
    for e in rd.flatten() {
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = e.file_name().to_string_lossy().to_string();
        // Служебные каталоги/файлы git-хранилища скрываем из просмотрщика.
        if name == ".git" || SERVICE_FILES.contains(&name.as_str()) {
            continue;
        }
        let rel = if path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", path.trim_matches('/'), name)
        };
        out.push(DevFileEntry {
            name,
            path: rel,
            size: md.len(),
            is_dir: md.is_dir(),
        });
    }
    // Как на GitHub: каталоги сверху, затем файлы, всё по алфавиту.
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// Содержимое файла для просмотрщика кода (README, исходники…).
#[tauri::command]
pub(crate) async fn devhub_read_file(
    app: tauri::AppHandle,
    username: String,
    repo: String,
    path: String,
) -> Result<DevFileContent, String> {
    let address = resolve_dev_username(&username).ok_or("Имя не найдено в реестре")?;
    let root = repo_dir(&app, &address, &repo);
    let file = safe_join(&root, &path)?;
    let md = std::fs::metadata(&file).map_err(|_| "Файл не найден".to_string())?;
    if md.is_dir() {
        return Err("Это каталог".to_string());
    }
    let size = md.len();
    let truncated = size > MAX_TEXT_FILE_BYTES;
    let bytes = if truncated {
        use std::io::Read;
        let mut f = std::fs::File::open(&file).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; MAX_TEXT_FILE_BYTES as usize];
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        buf.truncate(n);
        buf
    } else {
        std::fs::read(&file).map_err(|e| e.to_string())?
    };
    let binary = bytes.iter().take(8000).any(|b| *b == 0);
    let content = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    Ok(DevFileContent {
        content,
        size,
        binary,
        truncated,
    })
}

/// Есть ли локальная копия файлов репозитория (у чужих — false до P2P-синка).
#[tauri::command]
pub(crate) async fn devhub_files_local(app: tauri::AppHandle, username: String, repo: String) -> bool {
    resolve_dev_username(&username)
        .map(|a| repo_dir(&app, &a, &repo).exists())
        .unwrap_or(false)
}

/// SHA HEAD-коммита из рефов (main/master приоритетно).
fn head_of(refs: &[DevRef]) -> String {
    refs.iter()
        .find(|r| r.name == "refs/heads/main" || r.name == "refs/heads/master")
        .or_else(|| refs.iter().find(|r| r.name.starts_with("refs/heads/")))
        .or_else(|| refs.first())
        .map(|r| r.sha.clone())
        .unwrap_or_default()
}

/// Скачать файлы чужого репозитория от сидеров: swarm-загрузка bundle по хэшу
/// из подписанного индекса → checkout рабочей копии на диск. После этого
/// вкладка «Код» работает полностью локально (README, просмотр, диффы).
#[tauri::command]
pub(crate) async fn devhub_sync_files(
    app: tauri::AppHandle,
    username: String,
    repo: String,
) -> Result<(), String> {
    let address = resolve_dev_username(&username).ok_or("Имя не найдено в реестре")?;
    let dir = repo_dir(&app, &address, &repo);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bpath = ensure_bundle(&app, &username, &repo).await?;
    let meta = load_index(&app, &address)
        .and_then(|idx| idx.repos.into_iter().find(|m| m.name == repo));
    let head_sha = meta.as_ref().map(|m| head_of(&m.refs)).unwrap_or_default();
    if let Some(m) = &meta {
        let _ = std::fs::write(
            refs_path(&app, &address, &repo),
            serde_json::to_vec(&m.refs).unwrap_or_default(),
        );
    }
    let dir2 = dir.clone();
    tokio::task::spawn_blocking(move || crate::devhub_git::checkout_bundle(&dir2, &bpath, &head_sha))
        .await
        .map_err(|e| e.to_string())??;
    crate::syslog::log(
        "devhub",
        format!("files dev://{}/{} downloaded from seeders and unpacked", username, repo),
    );
    Ok(())
}

/// ⑂ Fork чужого репозитория к себе (как на GitHub): bundle и файлы копируются
/// на мой диск, репо попадает в МОЙ подписанный индекс с пометкой forked_from —
/// дальше работаю как со своим, оригинал автора не трогается.
#[tauri::command]
pub(crate) async fn devhub_fork_repo(
    app: tauri::AppHandle,
    username: String,
    repo: String,
) -> Result<String, String> {
    let repo = crate::dev_router::sanitize_segment("repo", &repo).map_err(|e| e.to_string())?;
    let my_name = crate::naming::get_my_username()
        .ok_or("Сначала зарегистрируйте имя пользователя (u://)")?;
    if my_name == username {
        return Err("Нельзя форкнуть собственный репозиторий".to_string());
    }
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован — разблокируйте PIN-кодом")?;
    let src_addr = resolve_dev_username(&username).ok_or("Имя автора не найдено в реестре")?;
    let src = load_index(&app, &src_addr)
        .and_then(|idx| idx.repos.into_iter().find(|m| m.name == repo))
        .ok_or_else(|| format!("репозиторий {}/{} не найден", username, repo))?;

    let mut index = load_local_index(&app, &address).unwrap_or(DevUserIndex {
        version: 1,
        username: my_name.clone(),
        address: address.clone(),
        pubkey: String::new(),
        seq: 0,
        updated: 0,
        repos: Vec::new(),
        starred: Vec::new(),
        prs_hash: String::new(),
        merged_prs: Vec::new(),
        sig: String::new(),
    });
    if index.repos.iter().any(|r| r.name == repo) {
        return Err(format!("У вас уже есть репозиторий «{}»", repo));
    }

    // Бандл источника: локальный кэш либо swarm-загрузка от сидеров.
    let src_bundle = ensure_bundle(&app, &username, &repo).await?;
    let dir = repo_dir(&app, &address, &repo);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bpath = bundle_path(&app, &address, &repo);
    std::fs::copy(&src_bundle, &bpath).map_err(|e| e.to_string())?;
    let refs = src.refs.clone();
    std::fs::write(
        refs_path(&app, &address, &repo),
        serde_json::to_vec(&refs).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // Распаковка рабочей копии (история коммитов — из клона, если git доступен).
    let mut log = src.log.clone();
    {
        let dir2 = dir.clone();
        let b2 = bpath.clone();
        let sha2 = head_of(&refs);
        if let Ok(Ok(l)) = tokio::task::spawn_blocking(move || {
            crate::devhub_git::checkout_bundle(&dir2, &b2, &sha2)
        })
        .await
        {
            if !l.is_empty() {
                log = l;
            }
        }
    }
    // Сидируем СВОЮ копию bundle — fork раздаётся независимо от оригинала.
    let bundle_hash = crate::p2p::seed_file(&app, &bpath.to_string_lossy())
        .await
        .unwrap_or_default();

    let now = now_secs();
    index.repos.push(DevRepoMeta {
        name: repo.clone(),
        description: src.description.clone(),
        license: src.license.clone(),
        language: src.language.clone(),
        default_branch: src.default_branch.clone(),
        branches: src.branches.clone(),
        created: now,
        updated: now,
        commits: src.commits,
        releases: Vec::new(),
        bundle_hash,
        refs,
        log,
        tree: build_tree_manifest(&dir),
        forked_from: format!("{}/{}", username, repo),
    });
    index.username = my_name.clone();
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event(&app, &address, &repo, "created");
    crate::syslog::log(
        "devhub",
        format!("fork created: dev://{}/{} ← {}/{} (seq {})", my_name, repo, username, repo, index.seq),
    );
    Ok(my_name)
}

/// Результат «Sync fork»: подтянули ли свежие коммиты из оригинального репо
/// в мой форк, сколько новых коммитов и SHA свежего HEAD (для UI-фидбэка).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevSyncForkResult {
    /// true → что-то было подтянуто; false → форк уже актуален.
    pub changed: bool,
    /// Сколько новых коммитов оригинала попало в мой форк (по SHA-окну лога).
    pub ahead_by: u32,
    /// SHA нового HEAD (default-ветка); пусто, если рефов нет.
    pub new_head: String,
    /// «username/repo» источника форка (для тоста в UI).
    pub source: String,
}

/// ⇅ Sync fork (как на GitHub): владелец форка подтягивает СВЕЖИЕ коммиты
/// оригинала (только коммиты + рабочие файлы + refs — releases НЕ трогаем,
/// это отдельные подписанные артефакты моего форка). Идентификация «новизны»
/// по HEAD SHA default-ветки: если совпадает — форк уже актуален. Иначе
/// swarm-загрузка bundle оригинала → checkout в мой каталог → пере-сидинг
/// СВОЕЙ копии bundle → seq+1 → переподпись → публикация.
#[tauri::command]
pub(crate) async fn devhub_sync_fork(
    app: tauri::AppHandle,
    repo: String,
) -> Result<DevSyncForkResult, String> {
    let repo = crate::dev_router::sanitize_segment("repo", &repo).map_err(|e| e.to_string())?;
    let my_name = crate::naming::get_my_username()
        .ok_or("Сначала зарегистрируйте имя пользователя (u://)")?;
    let address = my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован — разблокируйте PIN-кодом")?;

    // Мой репо → forked_from обязателен.
    let mut index = load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    let (fork_from, my_head_before, my_shas_before): (String, String, std::collections::HashSet<String>) = {
        let meta = index
            .repos
            .iter()
            .find(|m| m.name == repo)
            .ok_or_else(|| format!("Репозиторий «{}» не найден", repo))?;
        if meta.forked_from.is_empty() {
            return Err("Этот репозиторий не является форком".to_string());
        }
        (
            meta.forked_from.clone(),
            head_of(&meta.refs),
            meta.log.iter().map(|c| c.sha.clone()).collect(),
        )
    };
    let (src_user, src_repo) = fork_from
        .split_once('/')
        .ok_or("Битая метка forked_from")?;
    let src_repo_s = src_repo.to_string();
    let src_user_s = src_user.to_string();
    if src_user_s == my_name {
        return Err("Нельзя синхронизировать форк собственного репозитория".to_string());
    }

    // Явный клик пользователя → сбрасываем троттлинг синка автора-источника,
    // чтобы гарантированно получить свежайший подписанный индекс.
    let src_addr = resolve_dev_username(&src_user_s)
        .ok_or("Автор оригинального репозитория не найден в реестре")?;
    {
        let mut guard = SYNC_LAST.lock().unwrap();
        if let Some(map) = guard.as_mut() {
            map.remove(&src_addr);
        }
    }
    let _ = sync_remote_at(&app, &src_user_s, &src_addr).await;

    // Свежая мета источника.
    let src_meta = load_index(&app, &src_addr)
        .and_then(|idx| idx.repos.into_iter().find(|m| m.name == src_repo_s))
        .ok_or_else(|| format!("Оригинал {}/{} недоступен в сети", src_user_s, src_repo_s))?;
    let src_head = head_of(&src_meta.refs);

    // Уже актуально? Сравниваем HEAD default-ветки.
    if !src_head.is_empty() && src_head == my_head_before {
        return Ok(DevSyncForkResult {
            changed: false,
            ahead_by: 0,
            new_head: src_head,
            source: fork_from,
        });
    }
    if src_meta.bundle_hash.is_empty() {
        return Err("Оригинал ещё не опубликовал git-историю (bundle отсутствует)".to_string());
    }

    // Свежий bundle источника: сбрасываем кэш, если он существует, чтобы
    // ensure_bundle гарантированно скачал новую версию.
    let src_bpath = bundle_path(&app, &src_addr, &src_repo_s);
    let _ = std::fs::remove_file(&src_bpath);
    let src_bundle = ensure_bundle(&app, &src_user_s, &src_repo_s).await?;

    // Копируем в мой каталог + пишем свежие рефы + распаковываем рабочую копию.
    let dir = repo_dir(&app, &address, &repo);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bpath = bundle_path(&app, &address, &repo);
    std::fs::copy(&src_bundle, &bpath).map_err(|e| e.to_string())?;
    let refs = src_meta.refs.clone();
    std::fs::write(
        refs_path(&app, &address, &repo),
        serde_json::to_vec(&refs).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut log = src_meta.log.clone();
    {
        let dir2 = dir.clone();
        let b2 = bpath.clone();
        let sha2 = src_head.clone();
        if let Ok(Ok(l)) =
            tokio::task::spawn_blocking(move || crate::devhub_git::checkout_bundle(&dir2, &b2, &sha2)).await
        {
            if !l.is_empty() {
                log = l;
            }
        }
    }
    // Сидируем СВОЮ копию bundle — форк раздаётся независимо от оригинала.
    let bundle_hash = crate::p2p::seed_file(&app, &bpath.to_string_lossy())
        .await
        .unwrap_or_default();

    // Сколько новых коммитов подтянуто (окно лога = MAX_LOG, точнее не нужно).
    let ahead_by = log
        .iter()
        .take_while(|c| !my_shas_before.contains(&c.sha))
        .count() as u32;
    let last_msg = log.first().map(|c| c.message.clone()).unwrap_or_default();

    let now = now_secs();
    {
        let meta = index
            .repos
            .iter_mut()
            .find(|m| m.name == repo)
            .ok_or("Репозиторий исчез из индекса")?;
        meta.refs = refs;
        meta.log = log;
        meta.tree = build_tree_manifest(&dir);
        meta.commits = src_meta.commits;
        meta.branches = if src_meta.branches.is_empty() {
            meta.branches.clone()
        } else {
            src_meta.branches.clone()
        };
        meta.default_branch = if src_meta.default_branch.is_empty() {
            meta.default_branch.clone()
        } else {
            src_meta.default_branch.clone()
        };
        meta.bundle_hash = bundle_hash;
        meta.updated = now;
    }
    index.username = my_name;
    index.seq += 1;
    index.updated = now;
    sign_index(&mut index, &secret)?;
    save_local_index(&app, &index)?;
    publish_index(&app, &address).await;
    record_event_msg(&app, &address, &repo, "push", &last_msg);
    crate::syslog::log(
        "devhub",
        format!(
            "sync fork dev://{}/{} ← {} (+{} commits, seq {})",
            index.username, repo, fork_from, ahead_by, index.seq
        ),
    );
    Ok(DevSyncForkResult {
        changed: true,
        ahead_by,
        new_head: src_head,
        source: fork_from,
    })
}

