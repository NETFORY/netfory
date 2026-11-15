//! In-Memory VFS: VaultIndex (пути, чанки, дедупликация) + сериализация
//! в зашифрованный снапшот index.enc (bincode → ChaCha20-Poly1305).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::crypto;
use crate::error::VaultError;

/// Дефолтный размер чанка: 512 КБ — баланс HTTP-оверхеда (в 4 раза меньше
/// запросов, чем при 128 КБ), дедупликации и ZSTD-контекста.
pub const DEFAULT_CHUNK_SIZE: usize = 512 * 1024;

/// Верхняя граница распаковки одного чанка (защита от zstd-бомб).
/// Покрывает legacy-чанки 128 КБ и любые будущие значения CHUNK_SIZE.
pub const MAX_CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Размер чанка: env CHUNK_SIZE (в КБ, клампится в 64..=4096), иначе 512 КБ.
/// Читается один раз за процесс — менять на лету нельзя.
pub fn chunk_size() -> usize {
    static SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("CHUNK_SIZE")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .map(|kb| kb.clamp(64, 4096) * 1024)
            .unwrap_or(DEFAULT_CHUNK_SIZE)
    })
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileEntry {
    pub size: u64,
    pub created: i64,
    pub modified: i64,
    /// SHA-256 зашифрованных чанков, строго по порядку.
    pub chunks: Vec<String>,
    /// Файл сжат с пользовательским словарём user.dict.
    pub dict: bool,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct SavingsStats {
    /// Логический объём ТЕКУЩИХ файлов индекса (plaintext).
    pub raw_bytes: u64,
    /// Физически хранится в облаке сейчас (после ZSTD+шифрования).
    pub stored_bytes: u64,
    /// Сэкономлено дедупликацией (plaintext-байты чанков с общими ссылками).
    pub dedup_saved_bytes: u64,
}

/// Метаданные одного зашифрованного чанка — для точного пересчёта статистики.
#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct ChunkMeta {
    /// Размер plaintext-чанка до сжатия.
    pub plain: u64,
    /// Размер зашифрованного блоба в облаке.
    pub enc: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct VaultIndex {
    /// VFS-путь → метаданные файла.
    pub files: BTreeMap<String, FileEntry>,
    /// Дедупликация: SHA-256 plaintext-чанка → id зашифрованного чанка.
    pub dedup: HashMap<String, String>,
    /// Метрики экономии (пересчитываются из индекса при каждой мутации).
    #[serde(default)]
    pub stats: SavingsStats,
    /// Явно созданные (в т.ч. пустые) каталоги.
    #[serde(default)]
    pub dirs: BTreeSet<String>,
    /// id чанка → (plain, enc) байты. У legacy-индексов пуст — размеры
    /// оцениваются приближённо в recompute_stats.
    #[serde(default)]
    pub chunk_meta: HashMap<String, ChunkMeta>,
}

/// Формат индекса до добавления chunk_meta (bincode не переваривает
/// недостающие хвостовые поля — читаем старый снапшот отдельной структурой).
#[derive(Deserialize, Default)]
struct VaultIndexV1 {
    files: BTreeMap<String, FileEntry>,
    dedup: HashMap<String, String>,
    #[serde(default)]
    stats: SavingsStats,
    #[serde(default)]
    dirs: BTreeSet<String>,
}

#[derive(Debug)]
pub struct LsRow {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: i64,
}

impl VaultIndex {
    /// Нормализация VFS-пути: ведущий '/', без дублей '/', без хвостового '/'.
    pub fn normalize(path: &str) -> String {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        format!("/{}", parts.join("/"))
    }

    /// Листинг каталога прямо из In-Memory индекса (директории — виртуальные,
    /// выводятся из префиксов путей файлов).
    pub fn ls(&self, dir: &str) -> Vec<LsRow> {
        let dir = Self::normalize(dir);
        let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let mut rows: Vec<LsRow> = Vec::new();
        for (path, e) in &self.files {
            let Some(rest) = path.strip_prefix(&prefix) else { continue };
            if rest.is_empty() {
                continue;
            }
            match rest.split_once('/') {
                Some((d, _)) => {
                    dirs.insert(d.to_string());
                }
                None => rows.push(LsRow {
                    name: rest.to_string(),
                    is_dir: false,
                    size: e.size,
                    modified: e.modified,
                }),
            }
        }
        // явные (в т.ч. пустые) каталоги
        for d in &self.dirs {
            if let Some(rest) = d.strip_prefix(&prefix) {
                if !rest.is_empty() {
                    dirs.insert(rest.split('/').next().unwrap_or(rest).to_string());
                }
            }
        }
        let mut out: Vec<LsRow> = dirs
            .into_iter()
            .map(|d| LsRow { name: d, is_dir: true, size: 0, modified: 0 })
            .collect();
        out.extend(rows);
        out
    }

    pub fn to_encrypted(&self, key: &[u8; 32]) -> Result<Vec<u8>, VaultError> {
        let raw = bincode::serialize(self).map_err(|e| VaultError::Serde(e.to_string()))?;
        Ok(crypto::encrypt(key, &raw)?)
    }

    pub fn from_encrypted(key: &[u8; 32], data: &[u8]) -> Result<Self, VaultError> {
        let raw = crypto::decrypt(key, data)?;
        if let Ok(v) = bincode::deserialize::<Self>(&raw) {
            return Ok(v);
        }
        // Миграция: снапшот старого формата (без chunk_meta)
        let v1: VaultIndexV1 =
            bincode::deserialize(&raw).map_err(|e| VaultError::Serde(e.to_string()))?;
        let mut idx = VaultIndex {
            files: v1.files,
            dedup: v1.dedup,
            stats: v1.stats,
            dirs: v1.dirs,
            chunk_meta: HashMap::new(),
        };
        idx.recompute_stats();
        Ok(idx)
    }

    /// Точный пересчёт статистики из ТЕКУЩЕГО состояния индекса.
    /// Раньше счётчики только накапливались при push и не уменьшались при
    /// delete/перезаписи — RAW/IN CLOUD показывали объём «за всю жизнь».
    /// Для legacy-чанков без chunk_meta размеры оцениваются по файлам
    /// (enc ≈ plain — без учёта сжатия).
    pub fn recompute_stats(&mut self) {
        let raw: u64 = self.files.values().map(|e| e.size).sum();
        // Оценка plaintext-размера каждого referenced-чанка (fallback для legacy)
        let mut est: HashMap<&String, u64> = HashMap::new();
        for e in self.files.values() {
            let n = e.chunks.len() as u64;
            if n == 0 {
                continue;
            }
            let s = (e.size.div_ceil(n)).max(1);
            for (i, id) in e.chunks.iter().enumerate() {
                let plain =
                    if (i as u64) < n - 1 { s } else { e.size.saturating_sub(s * (n - 1)) };
                est.entry(id).or_insert(plain);
            }
        }
        let (mut stored, mut unique_plain) = (0u64, 0u64);
        for (id, plain_est) in &est {
            match self.chunk_meta.get(*id) {
                Some(m) => {
                    stored += m.enc;
                    unique_plain += m.plain;
                }
                None => {
                    stored += *plain_est;
                    unique_plain += *plain_est;
                }
            }
        }
        self.stats.raw_bytes = raw;
        self.stats.stored_bytes = stored;
        self.stats.dedup_saved_bytes = raw.saturating_sub(unique_plain);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_encrypted_roundtrip() {
        let key = [7u8; 32];
        let mut idx = VaultIndex::default();
        idx.files.insert(
            "/docs/readme.md".into(),
            FileEntry { size: 10, created: 1, modified: 2, chunks: vec!["ab".into()], dict: false },
        );
        let enc = idx.to_encrypted(&key).unwrap();
        let back = VaultIndex::from_encrypted(&key, &enc).unwrap();
        assert_eq!(back.files.len(), 1);
        assert!(back.files.contains_key("/docs/readme.md"));
    }

    #[test]
    fn ls_virtual_dirs() {
        let mut idx = VaultIndex::default();
        let e = FileEntry { size: 1, created: 0, modified: 0, chunks: vec![], dict: false };
        idx.files.insert("/a/b/file1.txt".into(), e.clone());
        idx.files.insert("/a/file2.txt".into(), e.clone());
        idx.files.insert("/root.txt".into(), e);
        let top = idx.ls("/");
        assert_eq!(top.len(), 2); // dir "a" + root.txt
        assert!(top[0].is_dir && top[0].name == "a");
        let a = idx.ls("/a");
        assert_eq!(a.len(), 2); // dir "b" + file2.txt
    }
}
