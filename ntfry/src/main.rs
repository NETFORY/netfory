//! ntfry CLI — In-Memory Encrypted Virtual File System.
//! Zero-Knowledge контейнер: BIP-39 → ZSTD (user.dict) → ChaCha20-Poly1305 →
//! Google Drive / Yandex Disk / локальный mock.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use ntfry::error::{Result, VaultError};
use ntfry::storage::gdrive::GoogleDriveProvider;
use ntfry::storage::local::LocalStorageProvider;
use ntfry::storage::yandex::YandexDiskProvider;
use ntfry::storage::StorageProvider;
use ntfry::vault::Vault;
use ntfry::{crypto, vfs};

#[derive(Parser)]
#[command(
    name = "ntfry",
    version,
    about = "ntfry — Zero-Knowledge Virtual Encrypted Cloud Storage (E2EE + ZSTD)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Провайдер хранилища: local | yandex | gdrive
    #[arg(long, global = true, env = "NTFRY_PROVIDER", default_value = "local")]
    provider: String,
    /// BIP-39 мнемоника (12/24 слова)
    #[arg(long, global = true, env = "NTFRY_MNEMONIC")]
    mnemonic: Option<String>,
    /// Корень локального mock-облака (для --provider local)
    #[arg(long, global = true, env = "NTFRY_LOCAL_ROOT", default_value = "./mock_cloud")]
    local_root: PathBuf,
    /// Каталог локального Sled-кэша индекса
    #[arg(long, global = true, env = "NTFRY_CACHE_DIR", default_value = "./cache")]
    cache_dir: PathBuf,
    /// Локальный ZSTD-словарь (переопределяет /ntfry/user.dict)
    #[arg(long, global = true, env = "NTFRY_DICT")]
    dict: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Генерация/валидация мнемоники + инициализация /ntfry/ на провайдере
    Init,
    /// Загрузка локального файла в VFS (чанки → сжатие → шифрование)
    Push {
        #[arg(long)]
        src: PathBuf,
        #[arg(long)]
        dst: String,
    },
    /// Скачивание файла из VFS (дешифрование → распаковка → сборка)
    Pull {
        #[arg(long)]
        src: String,
        #[arg(long)]
        dst: PathBuf,
    },
    /// Листинг файлов и каталогов из In-Memory индекса
    Ls {
        #[arg(long, default_value = "/")]
        path: String,
    },
    /// Создать (пустой) каталог в VFS
    Mkdir {
        #[arg(long)]
        path: String,
    },
    /// Удалить файл или каталог (рекурсивно) + GC осиротевших чанков
    Rm {
        #[arg(long)]
        path: String,
    },
    /// Обучить ZSTD-словарь (COVER) на файлах каталога → /ntfry/user.dict
    Dict {
        #[arg(long)]
        train_from: PathBuf,
        #[arg(long, default_value_t = 65536)]
        max_size: usize,
    },
}

fn make_provider(cli: &Cli) -> Result<Box<dyn StorageProvider>> {
    match cli.provider.as_str() {
        "local" => Ok(Box::new(LocalStorageProvider::new(&cli.local_root))),
        "yandex" => {
            let token = std::env::var("NTFRY_YANDEX_TOKEN").map_err(|_| {
                VaultError::Storage(ntfry::error::StorageError::MissingToken(
                    "задайте NTFRY_YANDEX_TOKEN (OAuth access token Yandex Disk)".into(),
                ))
            })?;
            Ok(Box::new(YandexDiskProvider::new(token)))
        }
        "gdrive" => {
            let token = std::env::var("NTFRY_GDRIVE_TOKEN").unwrap_or_default();
            Ok(Box::new(GoogleDriveProvider::new(token)))
        }
        other => Err(VaultError::Serde(format!(
            "неизвестный провайдер '{other}' (ожидается local|yandex|gdrive)"
        ))),
    }
}

/// Кэш изолируем по провайдеру, чтобы переключение local↔yandex не путало индексы.
fn cache_dir(cli: &Cli) -> PathBuf {
    cli.cache_dir.join(&cli.provider)
}

fn require_mnemonic(cli: &Cli) -> Result<crypto::KeyMaterial> {
    let phrase = cli.mnemonic.clone().ok_or_else(|| {
        VaultError::Crypto(ntfry::error::CryptoError::BadMnemonic(
            "мнемоника не задана: --mnemonic или env NTFRY_MNEMONIC".into(),
        ))
    })?;
    Ok(crypto::derive(&phrase)?)
}

fn load_local_dict(cli: &Cli) -> Result<Option<Vec<u8>>> {
    match &cli.dict {
        Some(p) => Ok(Some(std::fs::read(p)?)),
        None => Ok(None),
    }
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.2} MB", n as f64 / 1048576.0)
    }
}

async fn run(cli: Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Init => {
            let km = match &cli.mnemonic {
                Some(p) => crypto::derive(p)?,
                None => {
                    let phrase = crypto::generate_mnemonic();
                    println!("· Сгенерирована новая BIP-39 мнемоника (12 слов):\n");
                    println!("  {phrase}\n");
                    println!("  ⚠ СОХРАНИТЕ ФРАЗУ — это единственный ключ к данным (Zero-Knowledge).\n");
                    crypto::derive(&phrase)?
                }
            };
            println!("· Master Encryption Key (m/999'/0'/0'/0): {}", crypto::masked_hex(&km.master_key));
            println!("· Blockchain Address   (m/44'/60'/0'/0/0): {}", km.address);
            let storage = make_provider(&cli)?;
            let dict = load_local_dict(&cli)?;
            let vault = Vault::init(km.master_key, storage, &cache_dir(&cli), dict).await?;
            let _ = vault;
            println!("· Провайдер '{}': /ntfry/ создан, пустой index.enc загружен ✓", cli.provider);
        }
        Cmd::Push { src, dst } => {
            let km = require_mnemonic(&cli)?;
            let storage = make_provider(&cli)?;
            let dict = load_local_dict(&cli)?;
            let mut vault = Vault::open(km.master_key, storage, &cache_dir(&cli), dict).await?;
            let s = vault.push(src, dst, None).await?;
            println!(
                "push {} → {} ✓\n  {} · {} chunks ({} uploaded, {} dedup) · compressed {} ({:.0}%)",
                src.display(),
                vfs::VaultIndex::normalize(dst),
                fmt_bytes(s.size),
                s.chunks,
                s.uploaded,
                s.deduped,
                fmt_bytes(s.compressed_bytes),
                if s.size > 0 { s.compressed_bytes as f64 / s.size as f64 * 100.0 } else { 0.0 }
            );
            let st = &vault.index.stats;
            println!(
                "  vault totals: raw {} · stored {} · dedup saved {} · saved {:.0}%",
                fmt_bytes(st.raw_bytes),
                fmt_bytes(st.stored_bytes),
                fmt_bytes(st.dedup_saved_bytes),
                if st.raw_bytes > 0 { (1.0 - st.stored_bytes as f64 / st.raw_bytes as f64) * 100.0 } else { 0.0 }
            );
        }
        Cmd::Pull { src, dst } => {
            let km = require_mnemonic(&cli)?;
            let storage = make_provider(&cli)?;
            let dict = load_local_dict(&cli)?;
            let vault = Vault::open(km.master_key, storage, &cache_dir(&cli), dict).await?;
            let written = vault.pull(src, dst, None).await?;
            println!("pull {} → {} ✓ ({})", src, dst.display(), fmt_bytes(written));
        }
        Cmd::Ls { path } => {
            let km = require_mnemonic(&cli)?;
            let storage = make_provider(&cli)?;
            let vault = Vault::open(km.master_key, storage, &cache_dir(&cli), None).await?;
            if vault.synced_from_remote {
                println!("(индекс обновлён из облака)");
            }
            let rows = vault.index.ls(path);
            if rows.is_empty() {
                println!("{}: пусто", vfs::VaultIndex::normalize(path));
            }
            for r in rows {
                if r.is_dir {
                    println!("d  {:>10}  {}/", "-", r.name);
                } else {
                    println!("-  {:>10}  {}", fmt_bytes(r.size), r.name);
                }
            }
        }
        Cmd::Mkdir { path } => {
            let km = require_mnemonic(&cli)?;
            let storage = make_provider(&cli)?;
            let mut vault = Vault::open(km.master_key, storage, &cache_dir(&cli), None).await?;
            vault.mkdir(path).await?;
            println!("mkdir {} ✓", vfs::VaultIndex::normalize(path));
        }
        Cmd::Rm { path } => {
            let km = require_mnemonic(&cli)?;
            let storage = make_provider(&cli)?;
            let mut vault = Vault::open(km.master_key, storage, &cache_dir(&cli), None).await?;
            let n = vault.delete(path).await?;
            println!("rm {} ✓ ({} файлов, осиротевшие чанки собраны GC)", vfs::VaultIndex::normalize(path), n);
        }
        Cmd::Dict { train_from, max_size } => {
            let km = require_mnemonic(&cli)?;
            // сэмплы: до 256 файлов ≤1МБ каждый (только plaintext в RAM)
            let mut samples: Vec<Vec<u8>> = Vec::new();
            for entry in std::fs::read_dir(train_from)?.flatten().take(256) {
                let p = entry.path();
                if p.is_file() {
                    if let Ok(meta) = p.metadata() {
                        if meta.len() <= 1024 * 1024 {
                            if let Ok(data) = std::fs::read(&p) {
                                if !data.is_empty() {
                                    samples.push(data);
                                }
                            }
                        }
                    }
                }
            }
            if samples.len() < 8 {
                return Err(VaultError::Compression(format!(
                    "мало сэмплов ({}) — нужно ≥8 небольших файлов", samples.len()
                )));
            }
            let dict = Vault::train_dict(&samples, *max_size)?;
            let storage = make_provider(&cli)?;
            let mut vault = Vault::open(km.master_key, storage, &cache_dir(&cli), None).await?;
            vault.set_dict(dict.clone()).await?;
            println!("dict ✓ обучен на {} сэмплах, размер {} → /ntfry/user.dict", samples.len(), fmt_bytes(dict.len() as u64));
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("ошибка: {e}");
        std::process::exit(1);
    }
}
