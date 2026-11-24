// ============================================================
//  ai_telemetry::heartbeat — подписанный AI-heartbeat + AI Stats
//
//  Подпись: secp256k1 (SmartHoldem identity, как в seeding.rs).
//  Digest: sha256("staih|addr|ts|model|vramT|vramF|tflops*10|thr|ramT|ramF").
//  Формула digest ОБЯЗАНА совпадать с tracker/src/ai_pool.rs.
// ============================================================

use super::hw::HwSnapshot;
use crate::crypto::{keys_from_material, sha256};
use crate::SESSION_SEED;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Собрать и подписать тело heartbeat'а (None — кошелёк заблокирован).
pub(super) fn build_body(hw: &HwSnapshot, enabled: bool) -> Option<String> {
    let secret = { SESSION_SEED.lock().ok()?.as_ref()?.clone() };
    let (sk, pk_hex, address) = keys_from_material(secret.trim()).ok()?;
    let ts = now_secs();
    let (model, vt, vf, tf) = match &hw.gpu {
        Some(g) => (g.model.clone(), g.vram_total_mb, g.vram_free_mb, g.tflops_fp16),
        None => (String::new(), 0, 0, 0.0),
    };
    // TFLOPS в digest — целым числом ×10, чтобы исключить расхождения float-формата
    let t10 = (tf * 10.0).round() as u64;
    let c = &hw.cpu;
    let digest = sha256(
        format!(
            "staih|{address}|{ts}|{model}|{vt}|{vf}|{t10}|{}|{}|{}",
            c.threads, c.ram_total_mb, c.ram_free_mb
        )
        .as_bytes(),
    );
    let msg = secp256k1::Message::from_digest_slice(&digest).ok()?;
    let secp = secp256k1::Secp256k1::signing_only();
    let signature = hex::encode(secp.sign_ecdsa(&msg, &sk).serialize_compact());
    let gpu_json = hw.gpu.as_ref().map(|g| {
        serde_json::json!({
            "model": g.model,
            "vramTotalMb": g.vram_total_mb,
            "vramFreeMb": g.vram_free_mb,
            "tflopsFp16": g.tflops_fp16,
        })
    });
    Some(
        serde_json::json!({
            "nodeId": address,
            "timestamp": ts,
            "aiPoolEnabled": enabled,
            "publicKey": pk_hex,
            "signature": signature,
            "version": env!("CARGO_PKG_VERSION"),
            "metrics": {
                "gpu": gpu_json,
                "cpu": {
                    "threads": c.threads,
                    "ramTotalMb": c.ram_total_mb,
                    "ramFreeMb": c.ram_free_mb,
                },
            },
        })
        .to_string(),
    )
}

/// Разослать heartbeat на все AI-трекеры. true — хотя бы один принял.
pub(super) fn send(body: &str) -> bool {
    let mut ok = false;
    for t in crate::p2p::get_trackers_internal() {
        let url = format!("{}/v1/ai/heartbeat", crate::p2p::normalize_tracker(&t));
        let resp = ureq::post(&url)
            .timeout(Duration::from_secs(10))
            .set("Content-Type", "application/json")
            .send_string(body);
        if resp.is_ok() {
            ok = true;
        }
    }
    ok
}

/// Сетевые AI Stats: опрашиваем ВСЕ трекеры и берём самый «богатый» ответ
/// (макс. activeNodes, при равенстве — свежее updatedAt). Первый живой
/// трекер мог не получать heartbeat'ы нод и показал бы пустую сеть.
pub(super) fn fetch_net_stats() -> Result<serde_json::Value, String> {
    let mut best: Option<serde_json::Value> = None;
    for t in crate::p2p::get_trackers_internal() {
        let url = format!("{}/v1/ai/stats", crate::p2p::normalize_tracker(&t));
        let Ok(resp) = ureq::get(&url).timeout(Duration::from_secs(4)).call() else {
            continue;
        };
        let Ok(txt) = resp.into_string() else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        let better = match &best {
            None => true,
            Some(b) => {
                let (an, bn) = (
                    v["activeNodes"].as_u64().unwrap_or(0),
                    b["activeNodes"].as_u64().unwrap_or(0),
                );
                an > bn
                    || (an == bn
                        && v["updatedAt"].as_u64().unwrap_or(0) > b["updatedAt"].as_u64().unwrap_or(0))
            }
        };
        if better {
            best = Some(v);
        }
    }
    best.ok_or_else(|| "AI-трекеры недоступны".into())
}
