//! RaidProvider — RAID-режим: объединение N дисков в одно большое хранилище.
//! Размещение чанков — rendezvous-хэширование (HRW): для каждого chunk id
//! детерминированный приоритетный порядок членов, БЕЗ таблицы соответствий.
//! Скачивание/удаление пробуют членов в том же порядке (плюс остальных как
//! fallback), поэтому добавление нового диска не ломает старые файлы.
//! index.enc / meta / user.dict реплицируются на ВСЕ диски (чтение — с
//! первого доступного). Квота — сумма всех членов.

use async_trait::async_trait;

use super::{ResyncStats, StorageProvider};
use crate::crypto;
use crate::error::StorageError;

/// Тип RAID: Stripe — объединение объёма (RAID-0),
/// Mirror — зеркало на все члены (RAID-1, отказоустойчивость).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RaidMode {
    Stripe,
    Mirror,
}

pub struct RaidProvider {
    /// (стабильный ключ аккаунта, провайдер) — ключ участвует в HRW-хэше.
    members: Vec<(String, Box<dyn StorageProvider>)>,
    mode: RaidMode,
}

impl RaidProvider {
    pub fn new(members: Vec<(String, Box<dyn StorageProvider>)>, mode: RaidMode) -> Self {
        Self { members, mode }
    }

    /// HRW (rendezvous): индексы членов в приоритетном порядке для id.
    fn order(&self, id: &str) -> Vec<usize> {
        let mut scored: Vec<(String, usize)> = self
            .members
            .iter()
            .enumerate()
            .map(|(i, (key, _))| (crypto::sha256_hex(format!("{id}|{key}").as_bytes()), i))
            .collect();
        scored.sort();
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

#[async_trait]
impl StorageProvider for RaidProvider {
    async fn init_vault_dir(&self) -> Result<(), StorageError> {
        for (_, m) in &self.members {
            m.init_vault_dir().await?;
        }
        Ok(())
    }

    async fn upload_chunk(&self, id: &str, data: &[u8]) -> Result<(), StorageError> {
        // RAID-1: чанк обязан лечь на ВСЕ диски (иначе push падает)
        if self.mode == RaidMode::Mirror {
            for (_, m) in &self.members {
                m.upload_chunk(id, data).await?;
            }
            return Ok(());
        }
        // RAID-0: первый член HRW-порядка; при ошибке (полон/недоступен) — следующий
        let mut last = StorageError::NotFound("raid: нет членов".into());
        for i in self.order(id) {
            match self.members[i].1.upload_chunk(id, data).await {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn download_chunk(&self, id: &str) -> Result<Vec<u8>, StorageError> {
        let mut last = StorageError::NotFound(id.to_string());
        for i in self.order(id) {
            match self.members[i].1.download_chunk(id).await {
                Ok(d) => return Ok(d),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn upload_index(&self, encrypted_index: &[u8]) -> Result<(), StorageError> {
        // строгая согласованность: индекс обязан лечь на все диски
        for (_, m) in &self.members {
            m.upload_index(encrypted_index).await?;
        }
        Ok(())
    }

    async fn download_index(&self) -> Result<Vec<u8>, StorageError> {
        let mut last = StorageError::NotFound("index.enc".into());
        for (_, m) in &self.members {
            match m.download_index().await {
                Ok(d) => return Ok(d),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn remote_index_sha256(&self) -> Result<Option<String>, StorageError> {
        for (_, m) in &self.members {
            if let Ok(Some(s)) = m.remote_index_sha256().await {
                return Ok(Some(s));
            }
        }
        Ok(None)
    }

    async fn download_dict(&self) -> Result<Option<Vec<u8>>, StorageError> {
        for (_, m) in &self.members {
            if let Ok(Some(d)) = m.download_dict().await {
                return Ok(Some(d));
            }
        }
        Ok(None)
    }

    async fn upload_dict(&self, data: &[u8]) -> Result<(), StorageError> {
        for (_, m) in &self.members {
            m.upload_dict(data).await?;
        }
        Ok(())
    }

    async fn delete_vault(&self) -> Result<(), StorageError> {
        let mut first_err = None;
        for (_, m) in &self.members {
            if let Err(e) = m.delete_vault().await {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn delete_chunk(&self, id: &str) -> Result<(), StorageError> {
        // чанк живёт на одном из дисков — чистим везде (404 не ошибка)
        for (_, m) in &self.members {
            let _ = m.delete_chunk(id).await;
        }
        Ok(())
    }

    async fn download_meta(&self) -> Result<Option<Vec<u8>>, StorageError> {
        for (_, m) in &self.members {
            if let Ok(Some(d)) = m.download_meta().await {
                return Ok(Some(d));
            }
        }
        Ok(None)
    }

    async fn upload_meta(&self, data: &[u8]) -> Result<(), StorageError> {
        for (_, m) in &self.members {
            m.upload_meta(data).await?;
        }
        Ok(())
    }

    async fn quota(&self) -> Result<Option<(u64, u64)>, StorageError> {
        // Stripe: сумма всех. Mirror: полезный объём = диск с минимумом свободного.
        let mut got: Vec<(u64, u64)> = Vec::new();
        for (_, m) in &self.members {
            if let Ok(Some(q)) = m.quota().await {
                got.push(q);
            }
        }
        if got.is_empty() {
            return Ok(None);
        }
        Ok(Some(match self.mode {
            RaidMode::Stripe => got.iter().fold((0, 0), |a, q| (a.0 + q.0, a.1 + q.1)),
            RaidMode::Mirror => *got
                .iter()
                .min_by_key(|(t, u)| t.saturating_sub(*u))
                .expect("непустой список"),
        }))
    }

    async fn chunk_exists(&self, id: &str) -> Result<bool, StorageError> {
        for i in self.order(id) {
            if let Ok(true) = self.members[i].1.chunk_exists(id).await {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// RAID-1 re-sync: для каждого чанка находим членов без копии и докатываем
    /// данные с любого живого. Недоступные члены пропускаются (не валят процесс).
    async fn resync(
        &self,
        ids: &[String],
        progress: Option<&(dyn Fn(u64, u64) + Send + Sync)>,
    ) -> Result<ResyncStats, StorageError> {
        if self.mode != RaidMode::Mirror {
            return Err(StorageError::NotImplemented("re-sync применим только к RAID-1 (зеркало)".into()));
        }
        // восстановленный диск мог потерять /ntfry — пересоздаём идемпотентно
        for (_, m) in &self.members {
            let _ = m.init_vault_dir().await;
        }
        let total = ids.len() as u64;
        let mut st = ResyncStats::default();
        for (n, id) in ids.iter().enumerate() {
            let (mut have, mut missing) = (Vec::new(), Vec::new());
            for (i, (_, m)) in self.members.iter().enumerate() {
                match m.chunk_exists(id).await {
                    Ok(true) => have.push(i),
                    Ok(false) => missing.push(i),
                    Err(_) => {} // диск недоступен — чинить его сейчас нельзя
                }
            }
            st.checked += 1;
            if !missing.is_empty() {
                let mut data = None;
                for i in &have {
                    if let Ok(d) = self.members[*i].1.download_chunk(id).await {
                        data = Some(d);
                        break;
                    }
                }
                match data {
                    None => st.lost += 1,
                    Some(d) => {
                        for i in missing {
                            match self.members[i].1.upload_chunk(id, &d).await {
                                Ok(()) => st.copied += 1,
                                Err(_) => st.failed += 1,
                            }
                        }
                    }
                }
            }
            if let Some(cb) = progress {
                cb(n as u64 + 1, total);
            }
        }
        Ok(st)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::local::LocalStorageProvider;
    use crate::vault::Vault;
    use std::path::{Path, PathBuf};

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ntfry-raid-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn raid(a: &Path, b: &Path, mode: RaidMode) -> Box<dyn StorageProvider> {
        Box::new(RaidProvider::new(
            vec![
                ("acc-a".into(), Box::new(LocalStorageProvider::new(a)) as Box<dyn StorageProvider>),
                ("acc-b".into(), Box::new(LocalStorageProvider::new(b)) as Box<dyn StorageProvider>),
            ],
            mode,
        ))
    }

    fn chunk_count(root: &Path) -> usize {
        std::fs::read_dir(root.join("ntfry/chunks")).map(|rd| rd.count()).unwrap_or(0)
    }

    /// 2 диска → чанки распределяются по HRW, pull byte-identical, «второе
    /// устройство» видит файлы, delete чистит оба диска.
    #[tokio::test]
    async fn raid_two_drives_distribute_and_roundtrip() {
        let (r1, r2, cache) = (tmp("a"), tmp("b"), tmp("cache"));
        let key = [7u8; 32];
        // 9.5 чанков = 10 уникальных чанков
        let cs = crate::vfs::chunk_size();
        let mut data = vec![0u8; cs * 9 + cs / 2];
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 37 + i / 5) % 251) as u8;
        }
        let src = r1.join("src.bin");
        std::fs::write(&src, &data).unwrap();

        let mut v = Vault::init(key, raid(&r1, &r2, RaidMode::Stripe), &cache, None).await.unwrap();
        let st = v.push(&src, "/f.bin", None).await.unwrap();
        assert_eq!(st.chunks, 10);
        let (c1, c2) = (chunk_count(&r1), chunk_count(&r2));
        assert_eq!(c1 + c2, 10);
        assert!(c1 > 0 && c2 > 0, "чанки должны распределиться: {c1}/{c2}");
        // индекс реплицирован на оба диска
        assert!(r1.join("ntfry/index.enc").exists() && r2.join("ntfry/index.enc").exists());

        let out = r1.join("out.bin");
        v.pull("/f.bin", &out, None).await.unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data);

        // «второе устройство»: пустой кэш → индекс с любого члена
        let v2 = Vault::open(key, raid(&r1, &r2, RaidMode::Stripe), &tmp("cache2"), None).await.unwrap();
        assert_eq!(v2.index.files.len(), 1);
        let out2 = r1.join("out2.bin");
        v2.pull("/f.bin", &out2, None).await.unwrap();
        assert_eq!(std::fs::read(&out2).unwrap(), data);

        // delete → GC чистит чанки на ОБОИХ дисках
        v.delete("/f.bin").await.unwrap();
        assert_eq!(chunk_count(&r1) + chunk_count(&r2), 0);
    }

    /// RAID-1 (зеркало): каждый чанк лежит на ВСЕХ дисках, pull работает
    /// даже при полной потере одного из дисков.
    #[tokio::test]
    async fn raid_mirror_replicates_and_survives_drive_loss() {
        let (r1, r2, cache) = (tmp("m-a"), tmp("m-b"), tmp("m-cache"));
        let key = [9u8; 32];
        let mut data = vec![0u8; 600 * 1024]; // 5 чанков
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 13 + i / 7) % 249) as u8;
        }
        let src = r1.join("src.bin");
        std::fs::write(&src, &data).unwrap();

        let mut v = Vault::init(key, raid(&r1, &r2, RaidMode::Mirror), &cache, None).await.unwrap();
        let st = v.push(&src, "/m.bin", None).await.unwrap();
        // зеркало: полный набор чанков на КАЖДОМ диске
        assert_eq!(chunk_count(&r1), st.chunks);
        assert_eq!(chunk_count(&r2), st.chunks);

        // теряем диск A полностью — сейф остаётся читаемым через диск B
        std::fs::remove_dir_all(r1.join("ntfry")).unwrap();
        let v2 = Vault::open(key, raid(&r1, &r2, RaidMode::Mirror), &tmp("m-cache2"), None)
            .await
            .unwrap();
        let out = r2.join("out.bin");
        v2.pull("/m.bin", &out, None).await.unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data);
    }

    /// Re-sync деградированного зеркала: диск B теряет все данные → resync
    /// докатывает чанки и индекс с диска A, сейф читается ТОЛЬКО с диска B.
    #[tokio::test]
    async fn raid_mirror_resync_restores_lost_chunks() {
        let (r1, r2, cache) = (tmp("rs-a"), tmp("rs-b"), tmp("rs-cache"));
        let key = [3u8; 32];
        let mut data = vec![0u8; 500 * 1024]; // 4 чанка
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 11 + i / 3) % 247) as u8;
        }
        let src = r1.join("src.bin");
        std::fs::write(&src, &data).unwrap();

        let mut v = Vault::init(key, raid(&r1, &r2, RaidMode::Mirror), &cache, None).await.unwrap();
        let st = v.push(&src, "/r.bin", None).await.unwrap();
        // диск B теряет /ntfry целиком (чанки + индекс)
        std::fs::remove_dir_all(r2.join("ntfry")).unwrap();
        assert_eq!(chunk_count(&r2), 0);

        let rs = v.resync(None).await.unwrap();
        assert_eq!(rs.copied as usize, st.chunks);
        assert_eq!(rs.lost, 0);
        assert_eq!(rs.failed, 0);
        assert_eq!(chunk_count(&r2), st.chunks);
        assert!(r2.join("ntfry/index.enc").exists());

        // консистентность: зеркало полностью на месте — повторный resync ничего не копирует
        let rs2 = v.resync(None).await.unwrap();
        assert_eq!(rs2.copied, 0);

        // читаем сейф ТОЛЬКО с диска B (диск A потерян)
        std::fs::remove_dir_all(r1.join("ntfry")).unwrap();
        let v2 = Vault::open(key, raid(&r1, &r2, RaidMode::Mirror), &tmp("rs-cache2"), None)
            .await
            .unwrap();
        let out = r2.join("out.bin");
        v2.pull("/r.bin", &out, None).await.unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), data);
    }
}
