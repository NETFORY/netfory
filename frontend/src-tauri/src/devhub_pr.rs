// ============================================================
//  SmartNet // devhub_pr.rs — децентрализованные Pull Request'ы.
//
//  PR = подписанный ключом кошелька объект с ПОЛНЫМ содержимым изменённых
//  текстовых файлов (без самописного патч-парсера — надёжнее). Исходящие PR
//  автора собираются в prs.json, сидируются iroh-blob'ом, а хэш blob'а
//  публикуется в подписанном индексе автора (prs_hash). Получатель при
//  синке чужого индекса качает prs.json, проверяет подпись КАЖДОГО PR
//  (pubkey → address → on-chain имя) и складывает адресованные ему в Sled.
//  Принятие PR = запись файлов + новый git-commit/bundle + публикация id
//  в merged_prs индекса — контрибьютор видит статус при следующем синке.
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use serde::{Deserialize, Serialize};

use crate::{DB, SESSION_SEED};

const OUT_PREFIX: &str = "devhub:pr:out:";
const IN_PREFIX: &str = "devhub:pr:in:";
const SEEN_PREFIX: &str = "devhub:prs_seen:";

const MAX_PR_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_PR_BYTES: usize = 1024 * 1024;
const MAX_BLOB_BYTES: u64 = 4 * 1024 * 1024;

/// Один файл внутри PR: полное новое содержимое либо пометка удаления.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevPrFile {
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub deleted: bool,
}

/// Подписанный Pull Request (распространяется через prs.json автора).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DevPullRequest {
    pub id: String,
    pub author: String,
    pub address: String,
    pub pubkey: String,
    pub target_author: String,
    pub repo: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// SHA HEAD-коммита, от которого делались правки (информативно).
    #[serde(default)]
    pub base_sha: String,
    pub files: Vec<DevPrFile>,
    pub created: u64,
    pub sig: String,
    /// Локальный статус: open | merged | rejected (в сети игнорируется).
    #[serde(default)]
    pub status: String,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Сообщение подписи: покрывает адресата, репо, время и хэши содержимого.
fn signing_message(pr: &DevPullRequest) -> String {
    let files_json = serde_json::to_string(&pr.files).unwrap_or_default();
    format!(
        "devhub:pr:v1:{}:{}:{}:{}:{}:{}:{}",
        pr.address,
        pr.target_author,
        pr.repo,
        pr.created,
        hex::encode(crate::crypto::sha256(pr.title.as_bytes())),
        hex::encode(crate::crypto::sha256(pr.body.as_bytes())),
        hex::encode(crate::crypto::sha256(files_json.as_bytes())),
    )
}

/// Полная проверка чужого PR: pubkey → address → on-chain имя → подпись.
fn verify_pr(pr: &DevPullRequest) -> bool {
    let pk = match hex::decode(&pr.pubkey) {
        Ok(p) => p,
        Err(_) => return false,
    };
    if crate::crypto::address_from_pubkey(&pk) != pr.address {
        return false;
    }
    match crate::devhub::resolve_dev_username(&pr.author) {
        Some(addr) if addr == pr.address => {}
        _ => return false,
    }
    let msg = signing_message(pr);
    let hash = crate::crypto::sha256(msg.as_bytes());
    crate::sth_node::verify_schnorr_legacy(&pr.pubkey, &hash, &pr.sig)
}

fn scan(prefix: &str) -> Vec<DevPullRequest> {
    let mut out = Vec::new();
    for kv in DB.scan_prefix(prefix.as_bytes()).flatten() {
        if let Ok(pr) = serde_json::from_slice::<DevPullRequest>(&kv.1) {
            out.push(pr);
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// Безопасный путь файла внутри репозитория (никаких .. / абсолютных путей).
fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 200 {
        return Err("некорректный путь файла".to_string());
    }
    if path.starts_with('/') || path.contains("..") || path.contains('\\') {
        return Err(format!("недопустимый путь: {}", path));
    }
    if !path.chars().all(|c| c.is_ascii_alphanumeric() || "._-/ ".contains(c)) {
        return Err(format!("недопустимые символы в пути: {}", path));
    }
    Ok(())
}

/// Пересобрать prs.json из исходящих PR, засидировать и опубликовать
/// prs_hash в подписанном индексе (seq+1).
async fn publish_outgoing(app: &tauri::AppHandle) -> Result<(), String> {
    let username = crate::naming::get_my_username().ok_or("нет on-chain имени")?;
    let address = crate::devhub::my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let list = scan(OUT_PREFIX);
    let json = serde_json::to_vec(&list).map_err(|e| e.to_string())?;
    let dir = crate::devhub::user_dir(app, &address);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("prs.json");
    std::fs::write(&path, &json).map_err(|e| e.to_string())?;
    let hash = crate::p2p::seed_file(app, &path.to_string_lossy())
        .await
        .unwrap_or_default();
    let now = now_secs();
    let mut index = crate::devhub::load_local_index(app, &address).unwrap_or(
        crate::devhub::DevUserIndex {
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
        },
    );
    index.username = username;
    index.prs_hash = hash;
    index.seq += 1;
    index.updated = now;
    crate::devhub::sign_index(&mut index, &secret)?;
    crate::devhub::save_local_index(app, &index)?;
    crate::devhub::publish_index(app, &address).await;
    Ok(())
}

/// Создать и опубликовать подписанный Pull Request в чужой репозиторий.
#[tauri::command]
pub(crate) async fn devhub_create_pr(
    app: tauri::AppHandle,
    target_author: String,
    repo: String,
    title: String,
    body: String,
    base_sha: String,
    files: Vec<DevPrFile>,
) -> Result<DevPullRequest, String> {
    let title = title.trim().to_string();
    if title.is_empty() || title.len() > 140 {
        return Err("Заголовок PR: 1–140 символов".to_string());
    }
    if files.is_empty() || files.len() > MAX_PR_FILES {
        return Err(format!("PR должен менять от 1 до {} файлов", MAX_PR_FILES));
    }
    let mut total = 0usize;
    for f in &files {
        validate_path(&f.path)?;
        if f.content.len() > MAX_FILE_BYTES {
            return Err(format!("Файл {} больше 256 КБ", f.path));
        }
        total += f.content.len();
    }
    if total > MAX_PR_BYTES {
        return Err("Суммарный размер PR больше 1 МБ".to_string());
    }
    let username = crate::naming::get_my_username()
        .ok_or("Для PR нужно on-chain имя (u://) — оно подписывает ваш патч")?;
    if username == target_author {
        return Err("PR в собственный репозиторий не нужен — просто сделайте push".to_string());
    }
    let address = crate::devhub::my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let mut pr = DevPullRequest {
        id: String::new(),
        author: username,
        address,
        pubkey: String::new(),
        target_author,
        repo,
        title,
        body: body.trim().to_string(),
        base_sha,
        files,
        created: now_secs(),
        sig: String::new(),
        status: "open".to_string(),
    };
    let msg = signing_message(&pr);
    let (pubkey, _hash, sig) = crate::sth_node::sign_message(&secret, &msg)?;
    pr.pubkey = pubkey;
    pr.sig = sig;
    pr.id = blake3::hash(msg.as_bytes()).to_hex()[..16].to_string();
    let json = serde_json::to_vec(&pr).map_err(|e| e.to_string())?;
    DB.insert(format!("{}{}", OUT_PREFIX, pr.id).as_bytes(), json)
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    publish_outgoing(&app).await?;
    crate::syslog::log(
        "devhub",
        format!("PR «{}» → dev://{}/{} published (id {})", pr.title, pr.target_author, pr.repo, pr.id),
    );
    Ok(pr)
}

/// PR репозитория: входящие (если репо моё) либо мои исходящие (чужое репо,
/// статус merged подтягивается из подписанного индекса владельца).
#[tauri::command]
pub(crate) async fn devhub_list_prs(
    app: tauri::AppHandle,
    author: String,
    repo: String,
) -> Vec<DevPullRequest> {
    let me = crate::naming::get_my_username();
    if me.as_deref() == Some(author.as_str()) {
        return scan(IN_PREFIX)
            .into_iter()
            .filter(|p| p.repo == repo && p.target_author == author)
            .collect();
    }
    let merged: Vec<String> = crate::devhub::resolve_dev_username(&author)
        .and_then(|addr| crate::devhub::load_index(&app, &addr))
        .map(|i| i.merged_prs)
        .unwrap_or_default();
    scan(OUT_PREFIX)
        .into_iter()
        .filter(|p| p.repo == repo && p.target_author == author)
        .map(|mut p| {
            if merged.contains(&p.id) {
                p.status = "merged".to_string();
            }
            p
        })
        .collect()
}

/// Принять PR: записать файлы, собрать новый commit/bundle, опубликовать
/// merged-id в индексе — контрибьютор увидит статус при следующем синке.
#[tauri::command]
pub(crate) async fn devhub_accept_pr(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let key = format!("{}{}", IN_PREFIX, id);
    let mut pr: DevPullRequest = DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .ok_or("PR не найден")?;
    if pr.status == "merged" {
        return Err("PR уже принят".to_string());
    }
    let username = crate::naming::get_my_username().ok_or("нет on-chain имени")?;
    if pr.target_author != username {
        return Err("PR адресован не вам".to_string());
    }
    let address = crate::devhub::my_address().ok_or("Кошелёк не найден")?;
    let secret = SESSION_SEED
        .lock()
        .unwrap()
        .clone()
        .ok_or("Кошелёк заблокирован")?;
    let dir = crate::devhub::repo_dir(&app, &address, &pr.repo);
    if !dir.exists() {
        return Err(format!("репозиторий «{}» не найден", pr.repo));
    }
    for f in &pr.files {
        validate_path(&f.path)?;
    }
    let now = now_secs();
    let commit_msg = format!("Merge PR «{}» from @{}", pr.title, pr.author);
    let bpath = crate::devhub::bundle_path(&app, &address, &pr.repo);
    // Сначала пытаемся смёржить с СОХРАНЕНИЕМ полной git-истории (git CLI);
    // при недоступном git — fallback на одиночный снапшот bundle_from_dir.
    let git_try = if bpath.exists() {
        let head_sha = crate::devhub::repo_refs(&app, &address, &pr.repo)
            .first()
            .map(|r| r.sha.clone())
            .unwrap_or_default();
        let files_t: Vec<(String, String, bool)> = pr
            .files
            .iter()
            .map(|f| (f.path.clone(), f.content.clone(), f.deleted))
            .collect();
        let b2 = bpath.clone();
        let au = pr.author.clone();
        let cm = commit_msg.clone();
        tokio::task::spawn_blocking(move || {
            crate::devhub_git::merge_pr_bundle(&b2, &head_sha, &files_t, &au, &cm)
        })
        .await
        .map_err(|e| e.to_string())?
    } else {
        Err("bundle отсутствует".to_string())
    };
    let (bundle, commit_hex) = match git_try {
        Ok(v) => v,
        Err(e) => {
            crate::syslog::log(
                "devhub",
                format!("merge via git CLI failed ({e}) — fallback to snapshot"),
            );
            // Fallback: применяем файлы к рабочей директории и строим снапшот.
            for f in &pr.files {
                let p = dir.join(&f.path);
                if f.deleted {
                    let _ = std::fs::remove_file(&p);
                } else {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&p, f.content.as_bytes()).map_err(|e| e.to_string())?;
                }
            }
            crate::devhub_git::bundle_from_dir(&dir, &username, &commit_msg, now)?
        }
    };
    std::fs::write(&bpath, &bundle).map_err(|e| e.to_string())?;
    let refs = vec![crate::devhub::DevRef {
        sha: commit_hex,
        name: "refs/heads/main".to_string(),
    }];
    let refs_json = serde_json::to_vec(&refs).map_err(|e| e.to_string())?;
    std::fs::write(crate::devhub::refs_path(&app, &address, &pr.repo), refs_json)
        .map_err(|e| e.to_string())?;
    // Рабочие файлы + реальный git log из нового bundle (fallback внутри —
    // при недоступном git лог просто останется прежним).
    let merged_log: Vec<crate::devhub::DevCommit> = {
        let d2 = dir.clone();
        let b2 = bpath.clone();
        let s2 = refs[0].sha.clone();
        tokio::task::spawn_blocking(move || crate::devhub_git::checkout_bundle(&d2, &b2, &s2))
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or_default()
    };
    let bundle_hash = crate::p2p::seed_file(&app, &bpath.to_string_lossy())
        .await
        .unwrap_or_default();
    let mut index =
        crate::devhub::load_local_index(&app, &address).ok_or("Индекс DevHub не найден")?;
    if let Some(meta) = index.repos.iter_mut().find(|m| m.name == pr.repo) {
        meta.bundle_hash = bundle_hash;
        meta.refs = refs.clone();
        meta.updated = now;
        if !merged_log.is_empty() {
            // Реальная история из смёрженного bundle (git log).
            meta.commits = merged_log.len() as u32;
            meta.log = merged_log.clone();
        } else {
            meta.commits += 1;
            meta.log.insert(
                0,
                crate::devhub::DevCommit {
                    sha: refs[0].sha.clone(),
                    message: commit_msg.chars().take(200).collect(),
                    author: username.clone(),
                    date: now,
                },
            );
            meta.log.truncate(crate::devhub::MAX_LOG);
        }
    }
    if !index.merged_prs.contains(&pr.id) {
        index.merged_prs.push(pr.id.clone());
    }
    index.seq += 1;
    index.updated = now;
    crate::devhub::sign_index(&mut index, &secret)?;
    crate::devhub::save_local_index(&app, &index)?;
    crate::devhub::publish_index(&app, &address).await;
    crate::devhub::record_event_msg(&app, &address, &pr.repo, "push", &commit_msg);
    pr.status = "merged".to_string();
    if let Ok(j) = serde_json::to_vec(&pr) {
        let _ = DB.insert(key.as_bytes(), j);
        let _ = DB.flush();
    }
    crate::syslog::log(
        "devhub",
        format!("PR «{}» from @{} accepted into {} (seq {})", pr.title, pr.author, pr.repo, index.seq),
    );
    Ok(())
}

/// Отклонить PR (локальный статус; контрибьютору не транслируется).
#[tauri::command]
pub(crate) async fn devhub_reject_pr(id: String) -> Result<(), String> {
    let key = format!("{}{}", IN_PREFIX, id);
    let mut pr: DevPullRequest = DB
        .get(key.as_bytes())
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .ok_or("PR не найден")?;
    pr.status = "rejected".to_string();
    let json = serde_json::to_vec(&pr).map_err(|e| e.to_string())?;
    DB.insert(key.as_bytes(), json).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Число открытых входящих PR (бейдж DEV HUB в сайдбаре).
#[tauri::command]
pub(crate) async fn devhub_pr_open_count() -> u32 {
    let me = match crate::naming::get_my_username() {
        Some(m) => m,
        None => return 0,
    };
    scan(IN_PREFIX)
        .iter()
        .filter(|p| p.target_author == me && (p.status == "open" || p.status.is_empty()))
        .count() as u32
}

/// Хук синка: чужой индекс принёс prs_hash → качаем prs.json, верифицируем
/// каждый PR и складываем адресованные мне (вызывается из sync_remote).
pub(crate) async fn sync_incoming(app: &tauri::AppHandle, idx: &crate::devhub::DevUserIndex) {
    if idx.prs_hash.is_empty() {
        return;
    }
    let seen_key = format!("{}{}", SEEN_PREFIX, idx.address);
    let seen = DB
        .get(seen_key.as_bytes())
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(&v).to_string());
    if seen.as_deref() == Some(idx.prs_hash.as_str()) {
        return;
    }
    let me = match crate::naming::get_my_username() {
        Some(m) => m,
        None => return,
    };
    let (providers, _, _) = crate::p2p::devhub_discover(&idx.address).await;
    if providers.is_empty() {
        return;
    }
    let tmp = std::env::temp_dir().join(format!(
        "devprs-{}.json",
        &idx.prs_hash[..idx.prs_hash.len().min(12)]
    ));
    if crate::p2p::fetch_file(app, &providers[0], &idx.prs_hash, &tmp.to_string_lossy(), &providers[1..])
        .await
        .is_err()
    {
        return;
    }
    let raw = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    if raw.len() as u64 > MAX_BLOB_BYTES {
        return;
    }
    let list: Vec<DevPullRequest> = match serde_json::from_slice(&raw) {
        Ok(l) => l,
        Err(_) => return,
    };
    let mut got = 0u32;
    for mut pr in list {
        if pr.target_author != me || pr.address != idx.address {
            continue;
        }
        if pr.files.iter().any(|f| validate_path(&f.path).is_err()) {
            continue;
        }
        if !verify_pr(&pr) {
            continue;
        }
        let key = format!("{}{}", IN_PREFIX, pr.id);
        // Уже виденный PR не перетираем — сохраняем локальный статус.
        if DB.get(key.as_bytes()).ok().flatten().is_some() {
            continue;
        }
        pr.status = "open".to_string();
        if let Ok(j) = serde_json::to_vec(&pr) {
            let _ = DB.insert(key.as_bytes(), j);
            got += 1;
        }
    }
    let _ = DB.insert(seen_key.as_bytes(), idx.prs_hash.clone().into_bytes());
    let _ = DB.flush();
    if got > 0 {
        use tauri::Emitter;
        crate::syslog::log(
            "devhub",
            format!("received {} new Pull Requests from @{}", got, idx.username),
        );
        let _ = app.emit(
            "devhub-pr-received",
            serde_json::json!({ "from": idx.username, "count": got }),
        );
    }
}
