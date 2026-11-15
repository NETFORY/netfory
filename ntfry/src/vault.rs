//! Vault — движок конвейера: чанки 128КБ → ZSTD → ChaCha20-Poly1305 →
//! StorageProvider. Индекс живёт в RAM, кэшируется в Sled, синхронизируется
//! с облачным index.enc. Передача чанков — параллельная (до PARALLEL
//! одновременных транзакций). Никаких plaintext-временных файлов на диске.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::cache::Cache;
use crate::compression::Codec;
use crate::crypto;
use crate::error::{Result, VaultError};
use crate::storage::{with_retry, StorageProvider};
use crate::vfs::{chunk_size, FileEntry, VaultIndex};

/// Максимум одновременных передач чанков (upload/download).
pub const PARALLEL: usize = 6;

/// Колбэк прогресса: (обработано байт, всего байт).
pub type Progress<'a> = Option<&'a (dyn Fn(u64, u64) + Send + Sync)>;

pub struct PushStats {
    pub size: u64,
    pub chunks: usize,
    pub uploaded: usize,
    pub deduped: usize,
    pub compressed_bytes: u64,
}

pub struct Vault {
    key: [u8; 32],
    pub index: VaultIndex,
    storage: Arc<dyn StorageProvider>,
    cache: Cache,
    codec: Codec,
    /// Кэш был обновлён из облака при открытии.
    pub synced_from_remote: bool,
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn meta_json(key: &[u8; 32]) -> Vec<u8> {
    serde_json::json!({ "v": 1, "fp": crypto::key_fingerprint(key) })
        .to_string()
        .into_bytes()
}

/// Быстрая проверка отпечатка ключа из открытого /ntfry/meta.
/// Ok(true) — meta есть и совпадает; Ok(false) — meta отсутствует.
async fn check_meta(storage: &dyn StorageProvider, key: &[u8; 32]) -> Result<bool> {
    let Some(raw) = storage.download_meta().await.unwrap_or(None) else {
        return Ok(false);
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return Ok(false);
    };
    match v["fp"].as_str() {
        Some(fp) if fp == crypto::key_fingerprint(key) => Ok(true),
        Some(_) => Err(VaultError::KeyMismatch),
        None => Ok(false),
    }
}

impl Vault {
    /// Инициализация нового хранилища: /ntfry на провайдере + пустой index.enc.
    pub async fn init(
        key: [u8; 32],
        storage: Box<dyn StorageProvider>,
        cache_dir: &Path,
        dict: Option<Vec<u8>>,
    ) -> Result<Self> {
        let storage: Arc<dyn StorageProvider> = Arc::from(storage);
        storage.init_vault_dir().await?;
        storage.upload_meta(&meta_json(&key)).await?;
        if let Some(d) = &dict {
            storage.upload_dict(d).await?;
        }
        let cache = Cache::open(cache_dir).await?;
        let mut vault = Self {
            key,
            index: VaultIndex::default(),
            storage,
            cache,
            codec: Codec::new(dict),
            synced_from_remote: false,
        };
        vault.save_index().await?;
        Ok(vault)
    }

    /// Открытие: мгновенно из Sled, затем валидация против облака
    /// (сначала быстрый отпечаток ключа в /ntfry/meta, затем sha index.enc).
    pub async fn open(
        key: [u8; 32],
        storage: Box<dyn StorageProvider>,
        cache_dir: &Path,
        local_dict: Option<Vec<u8>>,
    ) -> Result<Self> {
        let storage: Arc<dyn StorageProvider> = Arc::from(storage);
        let cache = Cache::open(cache_dir).await?;
        // 0. Идемпотентно гарантируем /ntfry/ + быстрый детект «чужого ключа»
        storage.init_vault_dir().await?;
        let meta_ok = check_meta(&*storage, &key).await?;
        // 1. Мгновенная загрузка из локального кэша
        let mut index = match cache.load_index_enc().await {
            Some(enc) => VaultIndex::from_encrypted(&key, &enc).unwrap_or_default(),
            None => VaultIndex::default(),
        };
        // 2. Валидация против облака (дёшево: только хэш index.enc).
        // Различаем три ситуации:
        //  • Ok(Some(rsha)) — в облаке есть index.enc → штатная синхронизация;
        //  • Ok(None) — в облаке НЕТ index.enc:
        //      если раньше синхронизировались (synced_sha не пуст) → облако
        //      было очищено извне (юзер удалил /ntfry вручную из веб-UI),
        //      сбрасываем локальный Sled-кэш, иначе push/save_index воскресит
        //      удалённые файлы призраком старого индекса;
        //      если synced_sha пуст — первый запуск, ничего не делаем;
        //  • Err(_) — транзиентная сетевая ошибка, работаем офлайн из Sled.
        let mut synced = false;
        match storage.remote_index_sha256().await {
            Ok(Some(rsha)) => {
                if cache.synced_sha().await.as_deref() != Some(rsha.as_str()) {
                    // В облаке новая версия → качаем, обновляем RAM + Sled.
                    // AEAD-ошибка здесь = индекс зашифрован ЧУЖИМ ключом.
                    if let Ok(enc) = storage.download_index().await {
                        index = VaultIndex::from_encrypted(&key, &enc).map_err(|e| match e {
                            VaultError::Crypto(crate::error::CryptoError::Aead) => {
                                VaultError::KeyMismatch
                            }
                            other => other,
                        })?;
                        cache.save_index_enc(&enc).await?;
                        cache.set_synced_sha(&crypto::sha256_hex(&enc)).await?;
                        let _ = cache.set_stored_bytes(index.stats.stored_bytes).await;
                        synced = true;
                    }
                }
            }
            Ok(None) => {
                if cache.synced_sha().await.is_some() {
                    // Cloud reset detected — облако очистилось между запусками.
                    // Иначе призрачный индекс из Sled восстановит папку /ntfry
                    // при первом же push/mkdir/save_index.
                    cache.reset().await?;
                    index = VaultIndex::default();
                    synced = true;
                }
            }
            Err(_) => {
                // Оффлайн-режим: используем то, что есть в Sled-кэше, ничего
                // не сбрасываем. Пользователь просто «работает без сети».
            }
        }
        // self-heal: старый vault без meta — дописываем отпечаток
        if !meta_ok {
            let _ = storage.upload_meta(&meta_json(&key)).await;
        }
        // 3. Словарь: приоритет локального файла, иначе /ntfry/user.dict
        let dict = match local_dict {
            Some(d) => Some(d),
            None => storage.download_dict().await.unwrap_or(None),
        };
        // Самолечение статистики: legacy-счётчики копились «за всю жизнь»
        // и не уменьшались при удалении — пересчитываем из живого индекса.
        index.recompute_stats();
        Ok(Self {
            key,
            index,
            storage,
            cache,
            codec: Codec::new(dict),
            synced_from_remote: synced,
        })
    }

    /// PUSH: файл → чанки chunk_size() (дефолт 512КБ) → ZSTD → ChaCha20 →
    /// /ntfry/chunks/<sha256>. Дедуп по SHA-256 plaintext.
    /// Загрузка — до PARALLEL чанков одновременно.
    pub async fn push(&mut self, src: &Path, dst: &str, progress: Progress<'_>) -> Result<PushStats> {
        let dst = VaultIndex::normalize(dst);
        let mut f = tokio::fs::File::open(src).await?;
        let size = f.metadata().await?.len();
        let chunk = chunk_size();
        let mut buf = vec![0u8; chunk];
        let mut chunks: Vec<String> = Vec::new();
        let (mut uploaded, mut deduped) = (0usize, 0usize);
        let mut compressed_bytes = 0u64;
        let mut processed = 0u64;

        // пул одновременных загрузок; каждая future держит свой Arc на storage
        let mut inflight: FuturesUnordered<_> = FuturesUnordered::new();

        loop {
            // добираем чанк до полного размера (read может вернуть меньше)
            let mut filled = 0;
            while filled < chunk {
                let n = f.read(&mut buf[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            let plain = &buf[..filled];
            let plain_hash = crypto::sha256_hex(plain);
            if let Some(id) = self.index.dedup.get(&plain_hash) {
                chunks.push(id.clone());
                deduped += 1;
                processed += filled as u64;
                if let Some(cb) = progress {
                    cb(processed.min(size), size);
                }
            } else {
                let packed = self.codec.compress(plain)?;
                compressed_bytes += packed.len() as u64;
                let enc = crypto::encrypt(&self.key, &packed)?;
                let id = crypto::sha256_hex(&enc);
                self.index.chunk_meta.insert(
                    id.clone(),
                    crate::vfs::ChunkMeta { plain: filled as u64, enc: enc.len() as u64 },
                );
                self.index.dedup.insert(plain_hash, id.clone());
                chunks.push(id.clone());
                uploaded += 1;
                let st = self.storage.clone();
                let len = filled as u64;
                inflight.push(async move { with_retry(|| st.upload_chunk(&id, &enc)).await.map(|_| len) });
                if inflight.len() >= PARALLEL {
                    let done: u64 = inflight.next().await.expect("inflight непуст")?;
                    processed += done;
                    if let Some(cb) = progress {
                        cb(processed.min(size), size);
                    }
                }
            }
            if filled < chunk {
                break;
            }
        }
        // дожидаемся хвоста загрузок
        while let Some(r) = inflight.next().await {
            processed += r?;
            if let Some(cb) = progress {
                cb(processed.min(size), size);
            }
        }
        let now = now_ts();
        let created = self.index.files.get(&dst).map(|e| e.created).unwrap_or(now);
        let total = chunks.len();
        self.index.files.insert(
            dst,
            FileEntry {
                size,
                created,
                modified: now,
                chunks,
                dict: self.codec.has_dict(),
            },
        );
        self.index.recompute_stats();
        self.save_index().await?;
        Ok(PushStats { size, chunks: total, uploaded, deduped, compressed_bytes })
    }

    /// PULL: параллельное скачивание (до PARALLEL) с сохранением порядка
    /// чанков (buffered) → дешифровка → распаковка → запись в файл.
    pub async fn pull(&self, src: &str, dst: &Path, progress: Progress<'_>) -> Result<u64> {
        let src = VaultIndex::normalize(src);
        let entry = self
            .index
            .files
            .get(&src)
            .ok_or_else(|| VaultError::VfsNotFound(src.clone()))?;
        if entry.dict && !self.codec.has_dict() {
            return Err(VaultError::DictMissing);
        }
        if let Some(parent) = dst.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }
        let mut out = tokio::fs::File::create(dst).await?;
        let mut written = 0u64;
        let total = entry.size;
        // buffered() гоняет до PARALLEL скачиваний одновременно, но отдаёт
        // результаты строго по порядку — пишем в файл последовательно
        let storage = self.storage.clone();
        let mut stream = futures::stream::iter(entry.chunks.iter().cloned().map(move |id| {
            let st = storage.clone();
            async move { with_retry(|| st.download_chunk(&id)).await }
        }))
        .buffered(PARALLEL);
        while let Some(enc) = stream.next().await {
            let enc = enc?;
            let packed = crypto::decrypt(&self.key, &enc)?;
            let plain = self.codec.decompress(&packed)?;
            written += plain.len() as u64;
            out.write_all(&plain).await?;
            if let Some(cb) = progress {
                cb(written.min(total.max(written)), total);
            }
        }
        out.flush().await?;
        Ok(written)
    }

    /// Явное создание (пустого) каталога в VFS.
    pub async fn mkdir(&mut self, path: &str) -> Result<()> {
        let p = VaultIndex::normalize(path);
        if p == "/" {
            return Ok(());
        }
        self.index.dirs.insert(p);
        self.save_index().await
    }

    /// Удаление файла или каталога (рекурсивно) + GC осиротевших чанков.
    /// Возвращает число удалённых файлов.
    pub async fn delete(&mut self, path: &str) -> Result<usize> {
        let p = VaultIndex::normalize(path);
        let mut removed = 0usize;
        if self.index.files.remove(&p).is_some() {
            removed += 1;
        } else {
            let prefix = format!("{p}/");
            let keys: Vec<String> =
                self.index.files.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
            let had_dir = self.index.dirs.contains(&p)
                || self.index.dirs.iter().any(|d| d.starts_with(&prefix));
            if keys.is_empty() && !had_dir {
                return Err(VaultError::VfsNotFound(p));
            }
            for k in keys {
                self.index.files.remove(&k);
                removed += 1;
            }
            self.index.dirs.retain(|d| d != &p && !d.starts_with(&prefix));
        }
        // GC: чанки, на которые больше не ссылается ни один файл
        let referenced: HashSet<&String> =
            self.index.files.values().flat_map(|e| e.chunks.iter()).collect();
        let orphans: Vec<(String, String)> = self
            .index
            .dedup
            .iter()
            .filter(|(_, id)| !referenced.contains(id))
            .map(|(h, id)| (h.clone(), id.clone()))
            .collect();
        let mut gc: FuturesUnordered<_> = FuturesUnordered::new();
        let mut failed: Vec<String> = Vec::new();
        for (h, id) in orphans {
            let st = self.storage.clone();
            gc.push(async move {
                let r = st.delete_chunk(&id).await;
                (h, id, r)
            });
            if gc.len() >= PARALLEL {
                if let Some((h, id, r)) = gc.next().await {
                    match r {
                        Ok(()) => {
                            self.index.dedup.remove(&h);
                            self.index.chunk_meta.remove(&id);
                        }
                        Err(e) => failed.push(format!("{id}: {e}")),
                    }
                }
            }
        }
        while let Some((h, id, r)) = gc.next().await {
            match r {
                Ok(()) => {
                    self.index.dedup.remove(&h);
                    self.index.chunk_meta.remove(&id);
                }
                Err(e) => failed.push(format!("{id}: {e}")),
            }
        }
        self.index.recompute_stats();
        self.save_index().await?;
        if !failed.is_empty() {
            return Err(VaultError::GcFailed(format!(
                "{} чанк(ов) остались в облаке: {}",
                failed.len(),
                failed.join("; ")
            )));
        }
        Ok(removed)
    }

    /// Квота диска провайдера: (total, used) в байтах.
    pub async fn quota(&self) -> Option<(u64, u64)> {
        self.storage.quota().await.unwrap_or(None)
    }

    /// RAID-1 re-sync: докатка недостающих чанков на все члены зеркала,
    /// затем реплика index.enc и meta. Возвращает статистику ремонта.
    pub async fn resync(&mut self, progress: Progress<'_>) -> Result<crate::storage::ResyncStats> {
        let ids: Vec<String> = self
            .index
            .files
            .values()
            .flat_map(|e| e.chunks.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let stats = self.storage.resync(&ids, progress).await?;
        self.save_index().await?;
        let _ = self.storage.upload_meta(&meta_json(&self.key)).await;
        Ok(stats)
    }

    pub fn has_dict(&self) -> bool {
        self.codec.has_dict()
    }

    /// Обучение ZSTD-словаря (алгоритм COVER) на пользовательских сэмплах.
    pub fn train_dict(samples: &[Vec<u8>], max_size: usize) -> Result<Vec<u8>> {
        zstd::dict::from_samples(samples, max_size)
            .map_err(|e| VaultError::Compression(format!("обучение словаря: {e}")))
    }

    /// Установка пользовательского словаря. Защита: если в vault уже есть
    /// файлы, сжатые текущим словарём, замена сделает их нечитаемыми.
    pub async fn set_dict(&mut self, dict: Vec<u8>) -> Result<()> {
        if self.codec.has_dict() && self.index.files.values().any(|e| e.dict) {
            return Err(VaultError::Compression(
                "в vault есть файлы, сжатые текущим словарём — удалите или перезалейте их перед заменой".into(),
            ));
        }
        self.storage.upload_dict(&dict).await?;
        self.codec = Codec::new(Some(dict));
        Ok(())
    }

    /// Сначала Sled (локальная надёжность), затем асинхронный пуш в облако.
    async fn save_index(&mut self) -> Result<()> {
        let enc = self.index.to_encrypted(&self.key)?;
        self.cache.save_index_enc(&enc).await?;
        // Открытый хинт для UI «занято сейфом» — обновляем при каждом сейве.
        let _ = self.cache.set_stored_bytes(self.index.stats.stored_bytes).await;
        with_retry(|| self.storage.upload_index(&enc)).await?;
        self.cache.set_synced_sha(&crypto::sha256_hex(&enc)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorageProvider;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ntfry-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Чужой ключ против существующего облачного индекса → KeyMismatch
    /// (мгновенно, по отпечатку в /ntfry/meta, без скачивания индекса).
    #[tokio::test]
    async fn open_with_wrong_key_reports_key_mismatch() {
        let root = tmp("cloud-km");
        let cache = tmp("cache-km");
        let storage = Box::new(LocalStorageProvider::new(&root));
        let _v = Vault::init([1u8; 32], storage, &cache, None).await.unwrap();
        let storage2 = Box::new(LocalStorageProvider::new(&root));
        let cache2 = tmp("cache-km2");
        let err = match Vault::open([2u8; 32], storage2, &cache2, None).await {
            Err(e) => e,
            Ok(_) => panic!("ожидался KeyMismatch"),
        };
        assert!(matches!(err, VaultError::KeyMismatch));
        // delete_vault → повторный open с новым ключом проходит
        let storage3 = Box::new(LocalStorageProvider::new(&root));
        storage3.delete_vault().await.unwrap();
        let cache3 = tmp("cache-km3");
        let v = Vault::open([2u8; 32], storage3, &cache3, None).await.unwrap();
        assert_eq!(v.index.files.len(), 0);
    }

    /// mkdir + delete c GC осиротевших чанков.
    #[tokio::test]
    async fn mkdir_delete_and_chunk_gc() {
        let root = tmp("cloud-del");
        let cache = tmp("cache-del");
        let key = [5u8; 32];
        let cs = chunk_size();
        let data = vec![7u8; cs + cs / 2]; // 2 чанка
        let src = root.join("f.bin");
        std::fs::write(&src, &data).unwrap();

        let storage = Box::new(LocalStorageProvider::new(&root));
        let mut v = Vault::init(key, storage, &cache, None).await.unwrap();
        v.mkdir("/empty/dir").await.unwrap();
        assert!(v.index.ls("/").iter().any(|r| r.is_dir && r.name == "empty"));
        v.push(&src, "/docs/f.bin", None).await.unwrap();
        let chunk_count = std::fs::read_dir(root.join("ntfry/chunks")).unwrap().count();
        assert_eq!(chunk_count, 2);

        // удаление файла → чанки-сироты собраны GC
        assert_eq!(v.delete("/docs/f.bin").await.unwrap(), 1);
        let after = std::fs::read_dir(root.join("ntfry/chunks")).unwrap().count();
        assert_eq!(after, 0);
        assert!(v.index.dedup.is_empty());

        // удаление каталога
        v.delete("/empty").await.unwrap();
        assert!(!v.index.ls("/").iter().any(|r| r.name == "empty"));
    }

    /// Регрессия: статистика (RAW/IN CLOUD/SAVED) отражает ТЕКУЩЕЕ состояние,
    /// а не накопленный объём «за всю жизнь» — после удаления файлов цифры
    /// падают до фактического остатка.
    #[tokio::test]
    async fn stats_shrink_after_delete() {
        let root = tmp("cloud-stats");
        let cache = tmp("cache-stats");
        let key = [8u8; 32];
        let cs = chunk_size();
        let big = vec![1u8; cs * 3];
        let small = vec![2u8; cs / 4];
        let src_big = root.join("big.bin");
        let src_small = root.join("small.bin");
        std::fs::write(&src_big, &big).unwrap();
        std::fs::write(&src_small, &small).unwrap();

        let storage = Box::new(LocalStorageProvider::new(&root));
        let mut v = Vault::init(key, storage, &cache, None).await.unwrap();
        v.push(&src_big, "/big.bin", None).await.unwrap();
        v.push(&src_small, "/small.bin", None).await.unwrap();
        assert_eq!(v.index.stats.raw_bytes, (big.len() + small.len()) as u64);

        // удалили большой файл → raw/stored падают до остатка
        v.delete("/big.bin").await.unwrap();
        assert_eq!(v.index.stats.raw_bytes, small.len() as u64);
        assert!(v.index.stats.stored_bytes > 0);
        assert!(v.index.stats.stored_bytes < big.len() as u64);
        assert_eq!(v.index.stats.dedup_saved_bytes, 0);

        // удалили всё → нули
        v.delete("/small.bin").await.unwrap();
        assert_eq!(v.index.stats.raw_bytes, 0);
        assert_eq!(v.index.stats.stored_bytes, 0);
        assert_eq!(v.index.stats.dedup_saved_bytes, 0);
    }

    /// Обучение ZSTD-словаря + push/pull со словарём + защита от замены.
    #[tokio::test]
    async fn dict_train_set_and_roundtrip() {
        let root = tmp("cloud-dict");
        let cache = tmp("cache-dict");
        let key = [3u8; 32];
        // ≥8 похожих сэмплов для COVER
        let samples: Vec<Vec<u8>> = (0..16)
            .map(|i| format!("{{\"user\":\"user{i}\",\"balance\":{i},\"tags\":[\"smartnet\",\"vault\"]}}").repeat(40).into_bytes())
            .collect();
        let dict = Vault::train_dict(&samples, 65536).unwrap();
        assert!(!dict.is_empty());

        let storage = Box::new(LocalStorageProvider::new(&root));
        let mut v = Vault::init(key, storage, &cache, None).await.unwrap();
        v.set_dict(dict.clone()).await.unwrap();
        assert!(v.has_dict());

        let src = root.join("s.json");
        std::fs::write(&src, &samples[0]).unwrap();
        v.push(&src, "/s.json", None).await.unwrap();
        assert!(v.index.files["/s.json"].dict);

        // замена словаря при живых dict-файлах запрещена
        assert!(v.set_dict(vec![1, 2, 3]).await.is_err());

        // «второе устройство»: словарь подтягивается из /ntfry/user.dict
        let cache2 = tmp("cache-dict2");
        let storage2 = Box::new(LocalStorageProvider::new(&root));
        let v2 = Vault::open(key, storage2, &cache2, None).await.unwrap();
        assert!(v2.has_dict());
        let out = root.join("out.json");
        v2.pull("/s.json", &out, None).await.unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), samples[0]);
    }

    /// Полный E2E-раундтрип конвейера (параллельные передачи): Raw →
    /// Compress → Encrypt → upload → download → Decrypt → Decompress → SHA256.
    #[tokio::test]
    async fn full_pipeline_roundtrip_with_dedup() {
        let root = tmp("cloud");
        let cache1 = tmp("cache1");
        let key = [9u8; 32];

        // 9.5 чанков данных — проверяем параллельный порядок сборки
        let cs = chunk_size();
        let mut data = vec![0u8; cs * 9 + cs / 2];
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 31 + i / 7) % 251) as u8;
        }
        let src = root.join("src.bin");
        std::fs::write(&src, &data).unwrap();

        let storage = Box::new(LocalStorageProvider::new(&root));
        let mut vault = Vault::init(key, storage, &cache1, None).await.unwrap();
        let stats = vault.push(&src, "/docs/data.bin", None).await.unwrap();
        assert_eq!(stats.chunks, 10);
        assert_eq!(stats.uploaded, 10);

        // Повторный push того же файла → все чанки дедуплицированы
        let stats2 = vault.push(&src, "/copy/data2.bin", None).await.unwrap();
        assert_eq!(stats2.uploaded, 0);
        assert_eq!(stats2.deduped, 10);

        // Метрики экономии
        let s = &vault.index.stats;
        assert_eq!(s.raw_bytes, 2 * data.len() as u64);
        assert_eq!(s.dedup_saved_bytes, data.len() as u64);
        assert!(s.stored_bytes > 0);

        // Pull (параллельный, порядок чанков сохранён) и сверка SHA-256
        let out = root.join("out.bin");
        vault.pull("/docs/data.bin", &out, None).await.unwrap();
        let restored = std::fs::read(&out).unwrap();
        assert_eq!(crypto::sha256_hex(&data), crypto::sha256_hex(&restored));

        // «Второе устройство»: пустой кэш → индекс подтягивается из облака
        let cache2 = tmp("cache2");
        let storage2 = Box::new(LocalStorageProvider::new(&root));
        let vault2 = Vault::open(key, storage2, &cache2, None).await.unwrap();
        assert!(vault2.synced_from_remote);
        assert_eq!(vault2.index.files.len(), 2);
        let out2 = root.join("out2.bin");
        vault2.pull("/copy/data2.bin", &out2, None).await.unwrap();
        assert_eq!(std::fs::read(&out2).unwrap(), data);
    }

    /// Регрессия: облако было очищено извне (юзер удалил /ntfry вручную из
    /// веб-UI провайдера), а локальный Sled ещё держит старый индекс. При
    /// повторном открытии vault должен ЗАМЕТИТЬ пустое облако + непустой
    /// synced_sha и сбросить кэш, иначе «призрачный» индекс воскресит
    /// удалённые файлы при первом же push/save_index.
    #[tokio::test]
    async fn open_detects_external_cloud_wipe_and_resets_cache() {
        let root = tmp("cloud-wipe");
        let cache = tmp("cache-wipe");
        let key = [11u8; 32];
        let cs = chunk_size();
        let data = vec![42u8; cs + 100];
        let src = root.join("f.bin");
        std::fs::write(&src, &data).unwrap();
        // 1) Штатный сценарий: создаём vault + пушим файл → Sled запомнил
        //    synced_sha и зашифрованный индекс с одной записью.
        let mut v = Vault::init(
            key,
            Box::new(LocalStorageProvider::new(&root)),
            &cache,
            None,
        )
        .await
        .unwrap();
        v.push(&src, "/a.bin", None).await.unwrap();
        assert_eq!(v.index.files.len(), 1);
        // Дроп vault-а → Sled file-lock освобождается (Drop у sled::Db).
        drop(v);
        // 2) Пользователь заходит в веб-UI провайдера и удаляет папку /ntfry
        //    целиком. Симулируем — стираем облако:
        LocalStorageProvider::new(&root).delete_vault().await.unwrap();
        assert!(!root.join("ntfry").exists(), "облако должно быть пустым");
        // 3) Повторное открытие тем же ключом: облако пусто + Sled помнит →
        //    Vault::open должен САМ сбросить кэш и вернуть чистый vault.
        let v2 = Vault::open(
            key,
            Box::new(LocalStorageProvider::new(&root)),
            &cache,
            None,
        )
        .await
        .unwrap();
        assert_eq!(v2.index.files.len(), 0, "index.files должен быть пуст");
        assert!(v2.synced_from_remote, "должен быть выставлен признак sync");
        drop(v2);
        // 4) Ещё один open (без изменений облака): Sled уже чист, никаких
        //    призрачных данных не всплывёт. Заодно проверяем идемпотентность.
        let v3 = Vault::open(
            key,
            Box::new(LocalStorageProvider::new(&root)),
            &cache,
            None,
        )
        .await
        .unwrap();
        assert_eq!(v3.index.files.len(), 0);
    }
}
