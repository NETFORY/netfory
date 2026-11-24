// ============================================================
//  SmartNet // devhub_git.rs — генерация git-истории на чистом Rust.
//
//  Проблема: репозиторий, созданный через UI, содержал README/LICENSE только
//  как рабочие файлы на диске — git-истории не было, и `git clone dev://…`
//  отдавал пустой репозиторий. Решение (как на GitHub): клиент сам создаёт
//  настоящий initial commit — git-объекты (blob/tree/commit), packfile v2 и
//  bundle v2 собираются вручную, без зависимости от установленного git.
//
//  Форматы: object = sha1("<kind> <len>\0" + data); pack = "PACK"+v2+count+
//  (varint-заголовок + zlib(data))* + sha1-трейлер; bundle = "# v2 git bundle\n
//  <sha> refs/heads/main\n\n" + pack.
//
//  Author: TechnoL0g
//  Created: 2026-06
// ============================================================

use sha1::{Digest, Sha1};
use std::io::Write;
use std::path::Path;

const TYPE_COMMIT: u8 = 1;
const TYPE_TREE: u8 = 2;
const TYPE_BLOB: u8 = 3;
/// Максимальный размер файла, попадающего в initial commit (2 МБ — UI-репо крошечные).
const MAX_FILE: u64 = 2 * 1024 * 1024;

struct GitObj {
    kind: u8,
    data: Vec<u8>,
    sha: [u8; 20],
}

fn sha1_of(kind: &str, data: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(format!("{} {}\0", kind, data.len()).as_bytes());
    h.update(data);
    let d = h.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&d);
    out
}

fn blob(content: Vec<u8>) -> GitObj {
    let sha = sha1_of("blob", &content);
    GitObj { kind: TYPE_BLOB, data: content, sha }
}

/// Элемент дерева: (mode, имя, sha). Git сортирует записи по имени, причём
/// каталоги сравниваются как «имя/».
fn tree(mut entries: Vec<(&'static str, String, [u8; 20])>) -> GitObj {
    entries.sort_by(|a, b| {
        let ka = if a.0 == "40000" { format!("{}/", a.1) } else { a.1.clone() };
        let kb = if b.0 == "40000" { format!("{}/", b.1) } else { b.1.clone() };
        ka.as_bytes().cmp(kb.as_bytes())
    });
    let mut data = Vec::new();
    for (mode, name, sha) in entries {
        data.extend_from_slice(mode.as_bytes());
        data.push(b' ');
        data.extend_from_slice(name.as_bytes());
        data.push(0);
        data.extend_from_slice(&sha);
    }
    let sha = sha1_of("tree", &data);
    GitObj { kind: TYPE_TREE, data, sha }
}

fn commit(tree_sha: &[u8; 20], author: &str, ts: u64, message: &str) -> GitObj {
    let ident = format!("{} <{}@smartnet> {} +0000", author, author, ts);
    let data = format!(
        "tree {}\nauthor {}\ncommitter {}\n\n{}\n",
        hex::encode(tree_sha),
        ident,
        ident,
        message
    )
    .into_bytes();
    let sha = sha1_of("commit", &data);
    GitObj { kind: TYPE_COMMIT, data, sha }
}

/// Packfile v2: заголовок + (varint-хедер объекта + zlib(data))* + sha1-трейлер.
fn pack(objs: &[GitObj]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objs.len() as u32).to_be_bytes());
    for o in objs {
        // varint-заголовок: тип в битах 4-6 первого байта, размер по 4+7 бит.
        let mut size = o.data.len();
        let mut byte = ((o.kind << 4) as usize | (size & 0x0F)) as u8;
        size >>= 4;
        while size > 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7F) as u8;
            size >>= 7;
        }
        out.push(byte);
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&o.data).map_err(|e| e.to_string())?;
        out.extend_from_slice(&enc.finish().map_err(|e| e.to_string())?);
    }
    let mut h = Sha1::new();
    h.update(&out);
    out.extend_from_slice(&h.finalize());
    Ok(out)
}

/// Рекурсивно собрать дерево каталога (скрывая .git и служебные файлы devhub).
fn build_tree(dir: &Path, objs: &mut Vec<GitObj>) -> Result<Option<[u8; 20]>, String> {
    let mut entries: Vec<(&'static str, String, [u8; 20])> = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" || name == "repo.bundle" || name == "refs.json" {
            continue;
        }
        let md = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            if let Some(sha) = build_tree(&e.path(), objs)? {
                entries.push(("40000", name, sha));
            }
        } else if md.len() <= MAX_FILE {
            let content = std::fs::read(e.path()).map_err(|e2| e2.to_string())?;
            let b = blob(content);
            let sha = b.sha;
            objs.push(b);
            entries.push(("100644", name, sha));
        }
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let t = tree(entries);
    let sha = t.sha;
    objs.push(t);
    Ok(Some(sha))
}

/// Собрать git-bundle с ОДНИМ initial commit из рабочих файлов каталога.
/// Возвращает (байты bundle, hex-SHA коммита) — SHA идёт в refs подписанного
/// индекса, bundle сидируется в P2P и «git clone dev://…» отдаёт файлы.
pub(crate) fn bundle_from_dir(
    dir: &Path,
    author: &str,
    message: &str,
    ts: u64,
) -> Result<(Vec<u8>, String), String> {
    let mut objs: Vec<GitObj> = Vec::new();
    let tree_sha = build_tree(dir, &mut objs)?.ok_or("каталог пуст — нечего коммитить")?;
    let c = commit(&tree_sha, author, ts, message);
    let commit_hex = hex::encode(c.sha);
    // Коммит первым в pack'е (git это не требует, но так каноничнее).
    let mut ordered = vec![c];
    ordered.extend(objs);
    let mut bundle = format!(
        "# v2 git bundle\n{} refs/heads/main\n{} HEAD\n\n",
        commit_hex, commit_hex
    )
    .into_bytes();
    bundle.extend_from_slice(&pack(&ordered)?);
    Ok((bundle, commit_hex))
}

/// Извлечь рабочие файлы из git-bundle в каталог репозитория через локальный
/// git CLI. ВАЖНО: push-bundle от `git bundle create` НЕ содержит HEAD, поэтому
/// `git clone` оставляет рабочую копию ПУСТОЙ (exit 0!) — обязательно делаем
/// явный `checkout -f <head_sha>`. Возвращает историю коммитов (git log, до 50).
pub(crate) fn checkout_bundle(
    repo_dir: &Path,
    bundle: &Path,
    head_sha: &str,
) -> Result<Vec<crate::devhub::DevCommit>, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("devco-{}-{}", std::process::id(), nanos));
    let _ = std::fs::remove_dir_all(&tmp);
    let out = std::process::Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(bundle.as_os_str())
        .arg(tmp.as_os_str())
        .output()
        .map_err(|e| format!("git CLI недоступен: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // Явный checkout HEAD-коммита (bundle без HEAD → clone не делает checkout).
    if !head_sha.is_empty() {
        let co = std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.as_os_str())
            .arg("checkout")
            .arg("-f")
            .arg("--quiet")
            .arg(head_sha)
            .output()
            .map_err(|e| e.to_string())?;
        if !co.status.success() {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(format!(
                "git checkout {}: {}",
                &head_sha[..head_sha.len().min(7)],
                String::from_utf8_lossy(&co.stderr).trim()
            ));
        }
    }
    // История коммитов из клона (новые сверху, поля разделены \x01).
    let mut log: Vec<crate::devhub::DevCommit> = Vec::new();
    if let Ok(lo) = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.as_os_str())
        .arg("log")
        .arg("--format=%H%x01%an%x01%at%x01%s")
        .arg("-n")
        .arg(crate::devhub::MAX_LOG.to_string())
        .output()
    {
        if lo.status.success() {
            for line in String::from_utf8_lossy(&lo.stdout).lines() {
                let mut it = line.splitn(4, '\u{1}');
                let (Some(sha), Some(author), Some(at), Some(msg)) =
                    (it.next(), it.next(), it.next(), it.next())
                else {
                    continue;
                };
                log.push(crate::devhub::DevCommit {
                    sha: sha.to_string(),
                    message: msg.chars().take(200).collect(),
                    author: author.chars().take(60).collect(),
                    date: at.parse().unwrap_or(0),
                });
            }
        }
    }
    // Защита от пустого checkout: не трогаем старые файлы, если новых нет.
    let has_payload = std::fs::read_dir(&tmp)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy() != ".git")
        })
        .unwrap_or(false);
    if !has_payload {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err("checkout дал пустую рабочую копию — старые файлы сохранены".to_string());
    }
    // Старые рабочие файлы убираем (кроме сервисных repo.bundle/refs.json).
    if let Ok(rd) = std::fs::read_dir(repo_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".git" || name == "repo.bundle" || name == "refs.json" {
                continue;
            }
            let p = e.path();
            let _ = if p.is_dir() {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
    copy_tree(&tmp, repo_dir)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(log)
}

/// Принятие PR с СОХРАНЕНИЕМ полной git-истории: клонируем текущий bundle,
/// применяем файлы PR, делаем настоящий git-commit и пересобираем bundle со
/// всей историей. Требует git CLI; вызывающая сторона делает fallback на
/// bundle_from_dir (одиночный снапшот) при ошибке.
/// files: (path, content, deleted). Возвращает (bundle, sha нового коммита).
pub(crate) fn merge_pr_bundle(
    bundle: &Path,
    head_sha: &str,
    files: &[(String, String, bool)],
    author: &str,
    message: &str,
) -> Result<(Vec<u8>, String), String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("devmerge-{}-{}", std::process::id(), nanos));
    let _ = std::fs::remove_dir_all(&tmp);
    let run = |args: &[&str]| -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(args)
            .output()
            .map_err(|e| format!("git CLI недоступен: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let tmps = tmp.to_string_lossy().to_string();
    let bs = bundle.to_string_lossy().to_string();
    let res = (|| -> Result<(Vec<u8>, String), String> {
        run(&["clone", "--quiet", &bs, &tmps])?;
        // ветка main на HEAD-коммите (push-bundle может не иметь HEAD)
        if !head_sha.is_empty() {
            run(&["-C", &tmps, "checkout", "-q", "-B", "main", head_sha])?;
        } else {
            run(&["-C", &tmps, "checkout", "-q", "-B", "main"])?;
        }
        for (path, content, deleted) in files {
            let p = tmp.join(path);
            if *deleted {
                let _ = std::fs::remove_file(&p);
            } else {
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                std::fs::write(&p, content.as_bytes()).map_err(|e| e.to_string())?;
            }
        }
        run(&["-C", &tmps, "add", "-A"])?;
        let email = format!("{author}@smartnet");
        run(&[
            "-C", &tmps,
            "-c", &format!("user.name={author}"),
            "-c", &format!("user.email={email}"),
            "commit", "-q", "--allow-empty", "-m", message,
        ])?;
        let sha = run(&["-C", &tmps, "rev-parse", "HEAD"])?;
        let out_bundle = tmp.join("out.bundle");
        let obs = out_bundle.to_string_lossy().to_string();
        run(&["-C", &tmps, "bundle", "create", &obs, "main"])?;
        let bytes = std::fs::read(&out_bundle).map_err(|e| e.to_string())?;
        Ok((bytes, sha))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

/// Дифф коммита из bundle (`git show`) — рабочая копия не нужна, git читает
/// объекты прямо из клона. Вывод обрезается до 512 КБ.
pub(crate) fn diff_from_bundle(bundle: &Path, sha: &str) -> Result<String, String> {
    if !(7..=40).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("некорректный SHA коммита".to_string());
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("devdiff-{}-{}", std::process::id(), nanos));
    let _ = std::fs::remove_dir_all(&tmp);
    let out = std::process::Command::new("git")
        .arg("clone")
        .arg("--quiet")
        .arg(bundle.as_os_str())
        .arg(tmp.as_os_str())
        .output()
        .map_err(|e| format!("git CLI недоступен — установите git для просмотра диффов: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let show = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.as_os_str())
        .arg("show")
        .arg("--no-color")
        .arg("--stat=120")
        .arg("--patch")
        .arg(sha)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&tmp);
    if !show.status.success() {
        return Err(String::from_utf8_lossy(&show.stderr).trim().to_string());
    }
    let mut text = String::from_utf8_lossy(&show.stdout).to_string();
    if text.len() > 512 * 1024 {
        text.truncate(512 * 1024);
        text.push_str("\n… (дифф обрезан: больше 512 КБ)");
    }
    Ok(text)
}

/// Рекурсивное копирование дерева (без .git).
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    let rd = std::fs::read_dir(src).map_err(|e| e.to_string())?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let from = e.path();
        let to = dst.join(&name);
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
