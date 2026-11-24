// ============================================================
//  Author: TechnoL0g / Project: SmartNet Core / Date: 2026-07
//
//  swarm/source_iroh.rs — Боевой источник блоков поверх iroh-blobs.
//
//  Реализует `BlockSource`: скачивает ОДИН content-addressed blob (файл dApp) у
//  ОДНОГО конкретного пира, гейтит завершённость по `BlobStatus::Complete` (в
//  iroh-blobs 0.103 `download().await` резолвится в Ok даже если ничего не
//  передалось), затем экспортирует байты. Взаимозаменяем с моком в движке.
// ============================================================
#![cfg(feature = "p2p")]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use iroh::{Endpoint, EndpointId};
use iroh_blobs::api::proto::BlobStatus;
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::Hash;

use super::{BlockFuture, BlockHash, BlockSource, PeerId};

/// Уникализатор имён временных файлов экспорта (lock-free).
static EXPORT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Источник блоков поверх живого iroh-узла.
pub struct IrohBlockSource {
    store: FsStore,
    endpoint: Endpoint,
    tmp_dir: std::path::PathBuf,
}

impl IrohBlockSource {
    pub fn new(store: FsStore, endpoint: Endpoint, tmp_dir: std::path::PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&tmp_dir);
        Arc::new(Self { store, endpoint, tmp_dir })
    }
}

impl BlockSource for IrohBlockSource {
    fn fetch_block(&self, peer: PeerId, block: BlockHash) -> BlockFuture {
        let store = self.store.clone();
        let endpoint = self.endpoint.clone();
        let tmp_dir = self.tmp_dir.clone();
        Box::pin(async move {
            let nid: EndpointId = peer.parse().map_err(|_| "bad peer id".to_string())?;
            let h: Hash = block.parse().map_err(|_| "bad block hash".to_string())?;

            // Скачиваем до состояния Complete (3 попытки, гейт по статусу).
            let mut complete = matches!(
                store.blobs().status(h).await,
                Ok(BlobStatus::Complete { .. })
            );
            let mut last = String::from("провайдер не отдал данные");
            for _ in 0..3u32 {
                if complete {
                    break;
                }
                if let Err(e) = store.downloader(&endpoint).download(h, Some(nid)).await {
                    last = e.to_string();
                }
                match store.blobs().status(h).await {
                    Ok(BlobStatus::Complete { .. }) => complete = true,
                    Ok(BlobStatus::Partial { size }) => {
                        last = format!("частичный blob ({} байт)", size.unwrap_or(0));
                    }
                    Ok(BlobStatus::NotFound) => last = "blob отсутствует у пира".to_string(),
                    Err(e) => last = format!("status: {}", e),
                }
            }
            if !complete {
                return Err(last);
            }

            // Экспортируем байты блока во временный файл и читаем неблокирующе.
            let seq = EXPORT_SEQ.fetch_add(1, Ordering::Relaxed);
            let tmp = tmp_dir.join(format!("blk-{}-{}.bin", &block.get(..12).unwrap_or(&block), seq));
            store
                .blobs()
                .export(h, &tmp)
                .await
                .map_err(|e| e.to_string())?;
            let bytes = tokio::fs::read(&tmp).await.map_err(|e| e.to_string())?;
            let _ = tokio::fs::remove_file(&tmp).await;
            Ok(bytes)
        })
    }
}
