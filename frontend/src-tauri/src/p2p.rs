// ============================================================
//  SmartNet // p2p.rs
//
//  Peer-to-peer dApp distribution. Three cooperating layers:
//    1. LAN discovery (mDNS / `_smartnet._udp`) — announces this node and
//       the apps it seeds, and browses peers on the local network.
//    2. WAN bootstrap via signed HTTP trackers — heartbeat POST /announce +
//       GET /apps, merged into the same peer table (trackers hold only signed
//       {appId, endpointId, hash, version, meta}, never blobs).
//    3. iroh-blobs transit (optional `p2p` feature) — content-addressed
//       fetch/seed of each app folder; install exports to data/dapps/<id>
//       and `verify_app` proves integrity.
//  Also hosts the auto-update / relay-only / tracker-list preferences that
//  drive update propagation across the network.
//
//  Author: TechnoL0g
//  Created: 2026-06-18
// ============================================================

use crate::crypto::now_iso;
use crate::{ensure_data_dir, sanitize, Manifest, APP_PREFIX, DB};
#[cfg(feature = "p2p")]
use crate::{dir_size, get_storage_limit_internal};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

pub(crate) const SERVICE_TYPE: &str = "_smartnet._udp.local.";

/// PNG-логотипы каталога (блоб на приложение): жёсткий лимит размера и
/// magic-байты PNG — только такие блобы сидируются/принимаются.
pub(crate) const LOGO_MAX_BYTES: usize = 256 * 1024;
pub(crate) const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
pub(crate) const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// JPEG с арифметическим кодированием (SOF9-SOF11) — Chromium/WebView2 его
/// НЕ декодирует: такие превью выглядят как «пустая карточка». Отбраковываем.
pub(crate) fn jpeg_is_arithmetic(b: &[u8]) -> bool {
    let mut i = 2usize; // после FF D8
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            break;
        }
        let m = b[i + 1];
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        if m == 0xDA {
            break; // start-of-scan — сегменты заголовка закончились
        }
        if matches!(m, 0xC9 | 0xCA | 0xCB) {
            return true;
        }
        let len = ((b[i + 2] as usize) << 8) | b[i + 3] as usize;
        if len < 2 {
            break;
        }
        i += 2 + len;
    }
    false
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct PeerApp {
    id: String,
    hash: String,
    title: String,
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    // Content fingerprint (signed Blake3 root) + monotonic version of the
    // copy this peer is serving. Used to detect that a published update is
    // available for an app we already have installed.
    #[serde(default)]
    merkle: String,
    #[serde(default)]
    version: u32,
    /// epoch-seconds string of the serving copy's last publish/update ("" unknown).
    #[serde(default)]
    updated: String,
    /// tracker-reported store metadata (Phase B/D): payment + boost status.
    #[serde(default)]
    paid: bool,
    #[serde(rename = "boostPool", default)]
    boost_pool: u64,
    #[serde(rename = "priorityWeight", default)]
    priority_weight: u32,
    /// tracker-reported payload weight (bytes) + dynamic republish cost (STH).
    #[serde(rename = "sizeBytes", default)]
    size_bytes: u64,
    #[serde(default)]
    cost: u64,
    /// Адрес разработчика (owner из подписанного манифеста) — для фильтра
    /// Official Repository. Пустая строка у старых анонсов.
    #[serde(default)]
    owner: String,
    /// iroh-hash PNG-логотипа (отдельный блоб) — каталог тянет его лениво
    /// ДО установки; "" у старых анонсов / приложений без логотипа.
    #[serde(default)]
    logo: String,
    /// iroh-hash DAPP Preview 440×180 (PNG/JPG, отдельный блоб) — карточки
    /// каталога/дашборда; "" у старых анонсов / приложений без preview.
    #[serde(default)]
    preview: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct PeerInfo {
    node: String,
    #[serde(rename = "nodeId")]
    node_id: String,
    addr: String,
    apps: Vec<PeerApp>,
    #[serde(rename = "lastSeen")]
    last_seen: String,
    /// true = пир получен из Cloud Seeder fallback, а не по живому P2P.
    #[serde(rename = "viaSeeder", default)]
    via_seeder: bool,
}

pub(crate) static PEERS: Lazy<Mutex<HashMap<String, PeerInfo>>> = Lazy::new(|| Mutex::new(HashMap::new()));
pub(crate) static MDNS: Lazy<Mutex<Option<ServiceDaemon>>> = Lazy::new(|| Mutex::new(None));
pub(crate) static BROWSING: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

/// Собственная идентичность (wallet-адрес, iroh NodeId) — фильтр
/// самообнаружения: mDNS/трекеры/nodes.enc возвращают наш же анонс, и без
/// фильтра клиент видел «сам себя» как живого P2P-пира, из-за чего
/// cloud fallback никогда не запускался при полностью мёртвом P2P.
pub(crate) static OWN_IDENTITY: Lazy<Mutex<(String, String)>> =
    Lazy::new(|| Mutex::new((String::new(), String::new())));

/// ВСЕ раздаваемые dApp этой ноды: id → hash tar-блоба (результат seed_all).
/// В отличие от TRACKER_SNAPSHOT сюда попадают и бесплатные «только по ссылке»
/// приложения — источник данных для прямой ссылки sth://<id>?n=..&h=..
pub(crate) static SEEDED_APPS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// iroh-хэши PNG-логотипов раздаваемых dApp: id → hash блоба логотипа.
/// Заполняется в seed_all; анонсируется отдельной mDNS TXT-записью `l{i}`.
pub(crate) static SEEDED_LOGOS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// iroh-хэши DAPP Preview (440×180) раздаваемых dApp: id → hash блоба.
/// Заполняется в seed_all; анонсируется отдельной mDNS TXT-записью `p{i}`.
pub(crate) static SEEDED_PREVIEWS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// mDNS fullname → ключ в PEERS: нужен, чтобы по ServiceRemoved вычищать
/// умершие LAN-записи (иначе оффлайн-пиры навсегда блокировали cloud-режим).
static MDNS_FULLNAMES: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub(crate) fn is_self_peer(node: &str, node_id: &str) -> bool {
    let own = OWN_IDENTITY.lock().unwrap();
    (!own.1.is_empty() && own.1 == node_id)
        || (!own.0.is_empty() && own.0 != "anon" && own.0 == node)
}

/// (title, description, tags, merkle, version, updated) for a locally-known app id.
pub(crate) fn app_meta(id: &str) -> (String, String, Vec<String>, String, u32, String) {
    if let Ok(Some(v)) = DB.get(format!("{}{}", APP_PREFIX, id).as_bytes()) {
        if let Ok(m) = serde_json::from_slice::<Manifest>(&v) {
            return (
                m.name,
                m.description.unwrap_or_default(),
                m.tags,
                m.merkle,
                m.version.unwrap_or(1),
                m.updated.unwrap_or_default(),
            );
        }
    }
    (String::new(), String::new(), Vec::new(), String::new(), 0, String::new())
}

/// Адрес разработчика (owner подписанного манифеста) локального dApp.
pub(crate) fn app_owner(id: &str) -> String {
    if let Ok(Some(v)) = DB.get(format!("{}{}", APP_PREFIX, id).as_bytes()) {
        if let Ok(m) = serde_json::from_slice::<Manifest>(&v) {
            return m.owner;
        }
    }
    String::new()
}

/// nodeId сидеров official-dApps: appId → nodeIds (для vDapps-анонса в облако).
pub(crate) fn official_app_providers() -> HashMap<String, Vec<String>> {
    let devs: std::collections::HashSet<String> =
        crate::official::developers().into_iter().collect();
    let ids: std::collections::HashSet<String> =
        crate::official::official_dapps().into_iter().collect();
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for p in discovered_peers() {
        for a in &p.apps {
            let official = ids.contains(&a.id) || (!a.owner.is_empty() && devs.contains(&a.owner));
            if official && !p.node_id.is_empty() {
                let e = map.entry(a.id.clone()).or_default();
                if !e.contains(&p.node_id) {
                    e.push(p.node_id.clone());
                }
            }
        }
    }
    map
}

pub(crate) fn b64dec(s: &str) -> String {
    BASE64_STANDARD
        .decode(s)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

/// Start (or refresh) mDNS: announce this node — SmartHoldem address, iroh
/// NodeId, and one TXT entry per app `a{i} = id|blobHash|b64(title)|b64(desc)` —
/// and browse the LAN for other SmartNet nodes. Safe to call repeatedly: it
/// re-seeds + re-registers the (possibly changed) app set on every scan.
#[tauri::command]
pub(crate) async fn start_discovery(app: tauri::AppHandle) -> Result<String, String> {
    let node = crate::load_vault_internal()
        .map(|v| v.address)
        .unwrap_or_else(|| "anon".to_string());
    let instance = sanitize(&node);
    let host_name = format!("{}.local.", instance);

    let node_id = p2p_impl::node_id(&app).await.unwrap_or_default();
    *OWN_IDENTITY.lock().unwrap() = (node.clone(), node_id.clone());
    let seeded: HashMap<String, String> = p2p_impl::seed_all(&app)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    // Refresh the cross-app content-dedup index from what we currently seed.
    rebuild_dedup_index(&seeded);

    // WAN trackers: snapshot what we serve + ensure the heartbeat loop is running.
    *TRACKER_SNAPSHOT.lock().unwrap() = Some(build_tracker_snapshot(&node, &node_id, &seeded));
    start_tracker_thread();
    // медиа-сидеры мессенджера (headless seeder v1.5+): фоновое обнаружение
    crate::chat::media_seeders::start();

    let mut props: HashMap<String, String> = HashMap::new();
    props.insert("node".to_string(), node.clone());
    props.insert("nodeId".to_string(), node_id);
    for (i, id) in crate::announced_apps_internal().iter().enumerate() {
        let (title, desc, tags, _merkle, version, updated) = app_meta(id);
        let desc_t: String = desc.chars().take(80).collect();
        let hash = seeded.get(id).cloned().unwrap_or_default();
        // Владелец (owner манифеста) уходит отдельной TXT-записью `o{i}` —
        // формат `a{i}` не трогаем (обратная совместимость со старыми клиентами).
        let owner = app_owner(id);
        if !owner.is_empty() {
            props.insert(format!("o{}", i), owner);
        }
        // iroh-hash PNG-логотипа — отдельная TXT-запись `l{i}` (как owner в
        // `o{i}`): формат `a{i}` не трогаем, старые клиенты ключ игнорируют.
        if let Some(lh) = SEEDED_LOGOS.lock().unwrap().get(id.as_str()) {
            props.insert(format!("l{}", i), lh.clone());
        }
        // iroh-hash DAPP Preview (440×180) — TXT-запись `p{i}` (тот же паттерн)
        if let Some(ph) = SEEDED_PREVIEWS.lock().unwrap().get(id.as_str()) {
            props.insert(format!("p{}", i), ph.clone());
        }
        // NOTE: TXT-запись mDNS (key + '=' + value) обязана влезать в 255 байт:
        // oversize НЕ отбрасывается тихо — daemon-поток mdns-sd 0.11 ПАНИКУЕТ
        // (TryFromIntError, service_info.rs:647; постоянные PANIC в crash-логе).
        // Урезаем description (пополам за итерацию), затем tags; не влезло —
        // пропускаем анонс этого dApp целиком. Merkle сюда не кладём — длинный.
        let title_b64 = BASE64_STANDARD.encode(title.as_bytes());
        let mut desc_cap = desc_t.clone();
        let mut tags_s = tags.join(",");
        loop {
            let val = format!(
                "{}|{}|{}|{}|{}|{}|{}",
                id,
                hash,
                title_b64,
                BASE64_STANDARD.encode(desc_cap.as_bytes()),
                BASE64_STANDARD.encode(tags_s.as_bytes()),
                version,
                updated
            );
            if val.len() + 4 <= 250 {
                props.insert(format!("a{}", i), val);
                break;
            }
            if !desc_cap.is_empty() {
                let keep = desc_cap.chars().count() / 2;
                desc_cap = desc_cap.chars().take(keep).collect();
            } else if !tags_s.is_empty() {
                tags_s.clear();
            } else {
                break; // даже минимальная запись не влезает — не анонсируем
            }
        }
    }

    // get-or-create the daemon (ServiceDaemon is a cheap Arc handle)
    if MDNS.lock().unwrap().is_none() {
        let mdns = ServiceDaemon::new().map_err(|e| e.to_string())?;
        *MDNS.lock().unwrap() = Some(mdns);
    }
    let mdns = MDNS.lock().unwrap().as_ref().unwrap().clone();

    // (re)register our service — updates the announced app set on every scan
    if let Ok(si) = ServiceInfo::new(SERVICE_TYPE, &instance, &host_name, "", 7373, props) {
        let si = si.enable_addr_auto();
        let _ = mdns.register(si);
    }

    // browse only once for the process lifetime
    let start_browse = {
        let mut b = BROWSING.lock().unwrap();
        if *b {
            false
        } else {
            *b = true;
            true
        }
    };
    if start_browse {
        let receiver = mdns.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                // Умерший LAN-пир (TTL истёк / нода ушла): убираем из PEERS,
                // иначе оффлайн-запись навсегда держала has_direct_peers()=true
                // и блокировала cloud fallback.
                if let ServiceEvent::ServiceRemoved(_, fullname) = &event {
                    if let Some(key) = MDNS_FULLNAMES.lock().unwrap().remove(fullname) {
                        PEERS.lock().unwrap().remove(&key);
                    }
                    continue;
                }
                if let ServiceEvent::ServiceResolved(info) = event {
                    let props = info.get_properties();
                    let node = props.get_property_val_str("node").unwrap_or("").to_string();
                    let node_id = props
                        .get_property_val_str("nodeId")
                        .unwrap_or("")
                        .to_string();
                    // Свой же анонс вернулся браузеру — сам себе клиент не пир.
                    if is_self_peer(&node, &node_id) {
                        continue;
                    }
                    let mut apps: Vec<PeerApp> = Vec::new();
                    // отдельные TXT-записи `o{i}` несут owner-адрес приложения `a{i}`,
                    // `l{i}` — iroh-hash PNG-логотипа, `p{i}` — hash DAPP Preview
                    let mut owners: HashMap<String, String> = HashMap::new();
                    let mut logo_map: HashMap<String, String> = HashMap::new();
                    let mut preview_map: HashMap<String, String> = HashMap::new();
                    for p in props.iter() {
                        let key = p.key();
                        if key.len() >= 2
                            && key.starts_with('o')
                            && key[1..].chars().all(|c| c.is_ascii_digit())
                        {
                            owners.insert(key[1..].to_string(), p.val_str().to_string());
                        }
                        if key.len() >= 2
                            && key.starts_with('l')
                            && key[1..].chars().all(|c| c.is_ascii_digit())
                        {
                            logo_map.insert(key[1..].to_string(), p.val_str().to_string());
                        }
                        if key.len() >= 2
                            && key.starts_with('p')
                            && key[1..].chars().all(|c| c.is_ascii_digit())
                        {
                            preview_map.insert(key[1..].to_string(), p.val_str().to_string());
                        }
                    }
                    for p in props.iter() {
                        let key = p.key();
                        if key.len() >= 2
                            && key.starts_with('a')
                            && key[1..].chars().all(|c| c.is_ascii_digit())
                        {
                            let v = p.val_str();
                            let mut it = v.splitn(7, '|');
                            let id = it.next().unwrap_or("").to_string();
                            let hash = it.next().unwrap_or("").to_string();
                            let title = b64dec(it.next().unwrap_or(""));
                            let description = b64dec(it.next().unwrap_or(""));
                            let tags: Vec<String> = b64dec(it.next().unwrap_or(""))
                                .split(',')
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string())
                                .collect();
                            let version = it.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
                            let updated = it.next().unwrap_or("").to_string();
                            if !id.is_empty() {
                                let owner =
                                    owners.get(&key[1..]).cloned().unwrap_or_default();
                                let logo =
                                    logo_map.get(&key[1..]).cloned().unwrap_or_default();
                                let preview =
                                    preview_map.get(&key[1..]).cloned().unwrap_or_default();
                                apps.push(PeerApp {
                                    id,
                                    hash,
                                    title,
                                    description,
                                    tags,
                                    merkle: String::new(),
                                    version,
                                    updated,
                                    paid: false,
                                    boost_pool: 0,
                                    priority_weight: 0,
                                    size_bytes: 0,
                                    cost: 0,
                                    owner,
                                    logo,
                                    preview,
                                });
                            }
                        }
                    }
                    let addr = info
                        .get_addresses()
                        .iter()
                        .next()
                        .map(|a| format!("{}:{}", a, info.get_port()))
                        .unwrap_or_default();
                    let key = if node.is_empty() {
                        info.get_fullname().to_string()
                    } else {
                        node.clone()
                    };
                    MDNS_FULLNAMES
                        .lock()
                        .unwrap()
                        .insert(info.get_fullname().to_string(), key.clone());
                    PEERS.lock().unwrap().insert(
                        key,
                        PeerInfo {
                            node,
                            node_id,
                            addr,
                            apps,
                            last_seen: now_iso(),
                            via_seeder: false,
                        },
                    );
                }
            }
        });
    }
    // Phase 3b: авто-fallback на Cloud Seeder-ы, если P2P не оживает
    crate::cloudseeder::spawn_cloud_fallback();
    Ok("discovery-started".to_string())
}

#[tauri::command(async)]
pub(crate) fn discovered_peers() -> Vec<PeerInfo> {
    PEERS.lock().unwrap().values().cloned().collect()
}

/// Есть ли пиры, найденные по живому P2P (mDNS/трекеры), а не из cloud-fallback.
pub(crate) fn has_direct_peers() -> bool {
    PEERS.lock().unwrap().values().any(|p| !p.via_seeder)
}

/// Cloud Fallback: влить ноды из seeder-файла nodes.enc в таблицу PEERS
/// (ключи с префиксом `seeder:`, чтобы не затирать живые mDNS/трекер-записи).
pub(crate) fn merge_seeder_nodes(items: &serde_json::Value) -> usize {
    let Ok(peers) = serde_json::from_value::<Vec<PeerInfo>>(items.clone()) else {
        return 0;
    };
    let mut map = PEERS.lock().unwrap();
    let mut n = 0;
    for mut p in peers {
        let id = if p.node_id.is_empty() { p.node.clone() } else { p.node_id.clone() };
        if id.is_empty() {
            continue;
        }
        // nodes.enc чужого сидера может содержать снапшот НАС самих
        if is_self_peer(&p.node, &p.node_id) {
            continue;
        }
        p.via_seeder = true;
        p.last_seen = now_iso();
        map.insert(format!("seeder:{id}"), p);
        n += 1;
    }
    n
}

/// Cloud Fallback: dApps автора сидера как синтетический пир (via_seeder) —
/// они появляются на странице Discover с пометкой «via seeder».
pub(crate) fn merge_seeder_dapps(pubkey: &str, items: &serde_json::Value) -> usize {
    let Some(arr) = items.as_array() else { return 0 };
    let s = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    let apps: Vec<PeerApp> = arr
        .iter()
        .filter_map(|d| {
            let id = s(d, "id");
            if id.is_empty() {
                return None;
            }
            Some(PeerApp {
                id,
                hash: s(d, "blob"),
                title: s(d, "name"),
                description: s(d, "description"),
                tags: d
                    .get("tags")
                    .and_then(|t| serde_json::from_value(t.clone()).ok())
                    .unwrap_or_default(),
                merkle: s(d, "merkle"),
                version: d.get("version").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                updated: s(d, "updated"),
                paid: false,
                boost_pool: 0,
                priority_weight: 0,
                size_bytes: 0,
                cost: 0,
                owner: s(d, "owner"),
                // фаза 2 (v1.135.14): хэши каталожных медиа приходят в dapps.enc;
                // сами файлы клиент качает P2P либо из /ntfryseed/discovery_cache
                logo: s(d, "logo"),
                preview: s(d, "preview"),
            })
        })
        .collect();
    if apps.is_empty() {
        return 0;
    }
    let n = apps.len();
    let short = &pubkey[..pubkey.len().min(8)];
    PEERS.lock().unwrap().insert(
        format!("seeder:pub:{}", &pubkey[..pubkey.len().min(16)]),
        PeerInfo {
            node: format!("seed:{short}…"),
            node_id: String::new(),
            addr: String::new(),
            apps,
            last_seen: now_iso(),
            via_seeder: true,
        },
    );
    n
}

/// Known peer EndpointIds (from the tracker/mDNS directory) — used to bootstrap
/// the gossip overlay for the anti-censorship DHT-hint exchange.
pub(crate) fn known_endpoint_ids() -> Vec<String> {
    // ВАЖНО: возвращаем iroh EndpointId пиров (node_id), а НЕ ключи карты —
    // ключи это wallet-адреса и «tracker:<eid>»-строки, они не парсятся как
    // EndpointId, из-за чего gossip-бутстрап оставался пустым.
    let mut out: Vec<String> = PEERS
        .lock()
        .unwrap()
        .values()
        .map(|p| p.node_id.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[tauri::command]
pub(crate) fn stop_discovery() -> Result<(), String> {
    let mut guard = MDNS.lock().unwrap();
    if let Some(d) = guard.take() {
        let _ = d.shutdown();
    }
    PEERS.lock().unwrap().clear();
    MDNS_FULLNAMES.lock().unwrap().clear();
    Ok(())
}

// ============================================================
//  WAN discovery via HTTP trackers (bootstrap directory).
//  Heartbeat POST /announce + fetch GET /apps from each tracker and merge the
//  providers into PEERS alongside the LAN mDNS results. Trackers keep only
//  signed {appId, endpointId, hash, meta} in RAM with a TTL — never blobs.
// ============================================================

pub(crate) const DEFAULT_TRACKERS: &[&str] = &[
    "http://159.69.188.60:7374",
    "http://91.99.119.119:7374",
    "http://212.193.6.217:7374",
    "http://146.103.108.25:7374",
];

/// Default SmartNet DHT bootstrap seeder(s). Override via Network Settings
/// (`cfg:seeders`) or the `SMARTNET_SEEDERS` env. Multiple are supported.
pub(crate) const DEFAULT_SEEDERS: &[&str] = &[
    "116.202.32.250:6881",
    "194.58.109.118:6881",
];

#[derive(Debug, Serialize, Clone)]
pub(crate) struct TrackerAnnounceApp {
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    merkle: String,
    version: u32,
    hash: String,
    /// epoch-seconds string of the published copy's last update ("" unknown).
    #[serde(default)]
    updated: String,
    /// total payload size (bytes) — lets the tracker price publish/update
    /// validation against the physical weight of the indexed blobs.
    #[serde(rename = "size")]
    size: u64,
    signature: String,
    #[serde(rename = "publicKey")]
    public_key: String,
    /// Адрес разработчика (owner подписанного манифеста) — Official Repository.
    #[serde(default)]
    owner: String,
    /// iroh-hash PNG-логотипа (отдельный блоб, НЕ входит в подпись) — задел
    /// под фазу 2 (проброс через трекер); старые трекеры поле игнорируют.
    #[serde(skip_serializing_if = "String::is_empty")]
    logo: String,
    /// iroh-hash DAPP Preview 440×180 (PNG/JPG, НЕ входит в подпись) — задел
    /// под фазу 2; старые трекеры поле игнорируют.
    #[serde(skip_serializing_if = "String::is_empty")]
    preview: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TrackerAnnounceReq {
    node: String,
    #[serde(rename = "endpointId")]
    endpoint_id: String,
    apps: Vec<TrackerAnnounceApp>,
    /// A few live Mainline DHT nodes (ip:port) from our routing table, shared so
    /// censored peers (DHT=0, but tracker reachable) can bootstrap off them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dht: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TrackerProvider {
    #[serde(rename = "endpointId")]
    endpoint_id: String,
    #[serde(default)]
    node: String,
    #[serde(default)]
    hash: String,
    #[serde(default)]
    version: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TrackerApp {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    merkle: String,
    #[serde(default)]
    version: u32,
    #[serde(default)]
    updated: String,
    #[serde(rename = "boostPool", default)]
    boost_pool: u64,
    #[serde(rename = "priorityWeight", default)]
    priority_weight: u32,
    #[serde(default)]
    paid: bool,
    #[serde(rename = "sizeBytes", default)]
    size_bytes: u64,
    #[serde(default)]
    cost: u64,
    /// Адрес разработчика (owner манифеста), проброшенный трекером.
    #[serde(default)]
    owner: String,
    /// iroh-hash PNG-логотипа dApp, проброшенный трекером ("" у старых).
    #[serde(default)]
    logo: String,
    /// iroh-hash DAPP Preview dApp, проброшенный трекером ("" у старых).
    #[serde(default)]
    preview: String,
    #[serde(rename = "secondsRemaining", default)]
    seconds_remaining: u64,
    #[serde(rename = "bankSth", default)]
    bank_sth: u64,
    #[serde(default)]
    funding: Vec<TrackerFunding>,
    #[serde(default)]
    providers: Vec<TrackerProvider>,
}

/// Одно активное событие финансирования приложения (из tracker /apps).
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct TrackerFunding {
    #[serde(default)]
    id: String,
    #[serde(default)]
    ts: u64,
    #[serde(rename = "amountSmarthoshi", default)]
    amount_smarthoshi: String,
    #[serde(rename = "endsAt", default)]
    ends_at: u64,
}

/// Посуточная статистика сидера приложения (из tracker /app/{id}/seeders).
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct TrackerSeeder {
    #[serde(default)]
    address: String,
    #[serde(rename = "firstTs", default)]
    first_ts: u64,
    #[serde(default)]
    online: bool,
    #[serde(default)]
    days: std::collections::BTreeMap<u64, u64>,
}

#[derive(Clone)]
pub(crate) struct TrackerSnapshot {
    node: String,
    endpoint_id: String,
    apps: Vec<TrackerAnnounceApp>,
}

pub(crate) static TRACKER_SNAPSHOT: Lazy<Mutex<Option<TrackerSnapshot>>> = Lazy::new(|| Mutex::new(None));
pub(crate) static TRACKER_THREAD: Lazy<Mutex<bool>> = Lazy::new(|| Mutex::new(false));

pub(crate) fn normalize_tracker(t: &str) -> String {
    let t = t.trim().trim_end_matches('/');
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("http://{}", t)
    }
}

/// Broadcast an owner-signed deletion to every configured tracker (POST /delete).
/// Best-effort: failures are ignored. The tracker tombstones the app so it stops
/// being announced and headless seeders purge it via GET /deleted.
pub(crate) fn broadcast_delete(id: &str, public_key: &str, signature: &str, ts: u64) {
    let trackers = get_trackers_internal();
    let to = std::time::Duration::from_secs(5);
    let body = serde_json::json!({
        "id": id,
        "publicKey": public_key,
        "signature": signature,
        "ts": ts,
    });
    if let Ok(payload) = serde_json::to_string(&body) {
        for t in &trackers {
            let url = format!("{}/delete", normalize_tracker(t));
            let _ = ureq::post(&url)
                .timeout(to)
                .set("Content-Type", "application/json")
                .send_string(&payload);
        }
    }
}


pub(crate) fn get_trackers_internal() -> Vec<String> {
    if let Ok(Some(v)) = DB.get(b"cfg:trackers") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            let list: Vec<String> = s
                .lines()
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            if !list.is_empty() {
                return list;
            }
        }
    }
    DEFAULT_TRACKERS.iter().map(|s| s.to_string()).collect()
}

#[tauri::command]
pub(crate) fn get_trackers() -> Vec<String> {
    get_trackers_internal()
}

// ============================================================
//  Phase G — оценка начислений сидеру по сети (per-app payout model)
//
//  Клиент опрашивает трекеры (/apps + /app/{id}/seeders) и воспроизводит ту же
//  формулу, что и payout-сервер: банк каждого события финансирования делится на
//  5 дней, дневной пул — пропорционально онлайн-секундам eligible-сидеров, с
//  выпадением после 12ч офлайна. Это мотивационная оценка «начислено на данный
//  момент» — точная выплата приходит от payout-сервера.
// ============================================================

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DappEarning {
    id: String,
    name: String,
    #[serde(rename = "bankSth")]
    bank_sth: u64,
    #[serde(rename = "dailyPoolSth")]
    daily_pool_sth: f64,
    #[serde(rename = "secondsRemaining")]
    seconds_remaining: u64,
    #[serde(rename = "myOnlineSecs")]
    my_online_secs: u64,
    seeders: usize,
    online: bool,
    #[serde(rename = "estSth")]
    est_sth: f64,
    /// Прогноз доли за ТЕКУЩИЕ (ещё не закрытые) сутки при нынешних весах.
    #[serde(rename = "forecastSth")]
    forecast_sth: f64,
    /// Секунд до закрытия текущих суток (когда доля станет фактической).
    #[serde(rename = "nextPayoutSecs")]
    next_payout_secs: u64,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct EarningsSummary {
    #[serde(rename = "rewardAddress")]
    reward_address: String,
    #[serde(rename = "totalEstSth")]
    total_est_sth: f64,
    #[serde(rename = "activeApps")]
    active_apps: Vec<DappEarning>,
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn tracker_get_json<T: serde::de::DeserializeOwned>(url: &str) -> Option<T> {
    let to = std::time::Duration::from_secs(6);
    let resp = ureq::get(url).timeout(to).call().ok()?;
    let txt = resp.into_string().ok()?;
    serde_json::from_str::<T>(&txt).ok()
}

/// Оценить начисление одного адреса по одному приложению (в smarthoshi).
fn estimate_app_accrual(funding: &[TrackerFunding], seeders: &[TrackerSeeder], reward: &str, now: u64) -> u128 {
    const DAY: u64 = 86_400;
    const MAX_OFFLINE: u64 = 12 * 3600;
    let mut est: u128 = 0;
    for ev in funding {
        let bank: u128 = ev.amount_smarthoshi.parse().unwrap_or(0);
        if bank == 0 {
            continue;
        }
        let daily_pool = bank / 5;
        let d0 = ev.ts / DAY;
        let win_start = d0 * DAY;
        for k in 0..5u64 {
            let dk = d0 + k;
            let day_end = (dk + 1) * DAY;
            if now < day_end {
                continue; // день ещё не завершён
            }
            let mut total_w: u64 = 0;
            let mut my_w: u64 = 0;
            for s in seeders {
                let p_start = s.first_ts.max(win_start);
                let elapsed = now.min(day_end).saturating_sub(p_start);
                let mut online_total: u64 = 0;
                for j in 0..=k {
                    online_total += s.days.get(&(d0 + j)).copied().unwrap_or(0);
                }
                if elapsed.saturating_sub(online_total) > MAX_OFFLINE {
                    continue; // выпал по правилу 12ч
                }
                let w = s.days.get(&dk).copied().unwrap_or(0);
                if w > 0 {
                    total_w += w;
                    if s.address == reward {
                        my_w = w;
                    }
                }
            }
            if total_w > 0 && my_w > 0 {
                est += daily_pool * (my_w as u128) / (total_w as u128);
            }
        }
    }
    est
}

/// Прогноз доли за ТЕКУЩИЕ (ещё не закрытые) сутки при нынешних весах сидеров,
/// плюс сколько секунд осталось до закрытия этих суток (когда доля станет
/// фактической). Возвращает (forecast_smarthoshi, next_payout_secs).
///
/// Нужно, чтобы не показывать пугающий «≈ 0.0000», пока первые сутки окна ещё
/// не завершились: до закрытия дня фактическое начисление честно равно 0.
fn forecast_current_day(
    funding: &[TrackerFunding],
    seeders: &[TrackerSeeder],
    reward: &str,
    now: u64,
) -> (u128, u64) {
    const DAY: u64 = 86_400;
    const MAX_OFFLINE: u64 = 12 * 3600;
    let mut forecast: u128 = 0;
    let mut next_payout: u64 = 0;
    for ev in funding {
        let bank: u128 = ev.amount_smarthoshi.parse().unwrap_or(0);
        if bank == 0 {
            continue;
        }
        let daily_pool = bank / 5;
        let d0 = ev.ts / DAY;
        let win_start = d0 * DAY;
        let today = now / DAY;
        // Текущие сутки должны попадать в 5-дневное окно.
        if today < d0 || today >= d0 + 5 {
            continue;
        }
        let k = today - d0;
        let day_end = (today + 1) * DAY;
        let mut total_w: u64 = 0;
        let mut my_w: u64 = 0;
        for s in seeders {
            let p_start = s.first_ts.max(win_start);
            let elapsed = now.min(day_end).saturating_sub(p_start);
            let mut online_total: u64 = 0;
            for j in 0..=k {
                online_total += s.days.get(&(d0 + j)).copied().unwrap_or(0);
            }
            if elapsed.saturating_sub(online_total) > MAX_OFFLINE {
                continue; // выпал по правилу 12ч
            }
            let w = s.days.get(&today).copied().unwrap_or(0);
            if w > 0 {
                total_w += w;
                if s.address == reward {
                    my_w = w;
                }
            }
        }
        if total_w > 0 && my_w > 0 {
            forecast += daily_pool * (my_w as u128) / (total_w as u128);
        }
        let secs = day_end.saturating_sub(now);
        if next_payout == 0 || secs < next_payout {
            next_payout = secs;
        }
    }
    (forecast, next_payout)
}

#[tauri::command]
pub(crate) async fn network_earnings(reward_address: String) -> EarningsSummary {
    // Блокирующие ureq-запросы к трекерам НЕЛЬЗЯ выполнять на главном потоке —
    // это вешало окно на ~10с. Выносим в пул spawn_blocking.
    tauri::async_runtime::spawn_blocking(move || network_earnings_blocking(reward_address))
        .await
        .unwrap_or_else(|_| EarningsSummary {
            reward_address: String::new(),
            total_est_sth: 0.0,
            active_apps: Vec::new(),
        })
}

fn network_earnings_blocking(reward_address: String) -> EarningsSummary {
    let reward = reward_address.trim().to_string();
    let now = now_unix_secs();
    // директория приложений: берём запись с наибольшим числом funding-событий
    let mut by_id: std::collections::HashMap<String, TrackerApp> = std::collections::HashMap::new();
    for t in get_trackers_internal() {
        let url = format!("{}/apps", normalize_tracker(&t));
        if let Some(apps) = tracker_get_json::<Vec<TrackerApp>>(&url) {
            for a in apps {
                let better = by_id
                    .get(&a.id)
                    .map(|p| a.funding.len() > p.funding.len())
                    .unwrap_or(true);
                if better {
                    by_id.insert(a.id.clone(), a);
                }
            }
        }
    }
    let trackers = get_trackers_internal();
    let mut active: Vec<DappEarning> = Vec::new();
    let mut total_est: f64 = 0.0;
    for (id, app) in by_id.iter() {
        let has_active = app.funding.iter().any(|f| f.ends_at > now);
        if !has_active {
            continue;
        }
        // сливаем статистику сидеров по трекерам (макс. секунд на день, ранний first_ts)
        let mut merged: std::collections::HashMap<String, TrackerSeeder> = std::collections::HashMap::new();
        for t in &trackers {
            let url = format!("{}/app/{}/seeders", normalize_tracker(t), id);
            if let Some(list) = tracker_get_json::<Vec<TrackerSeeder>>(&url) {
                for s in list {
                    let e = merged.entry(s.address.clone()).or_insert_with(|| TrackerSeeder {
                        address: s.address.clone(),
                        first_ts: s.first_ts,
                        online: false,
                        days: Default::default(),
                    });
                    if s.first_ts > 0 {
                        e.first_ts = if e.first_ts == 0 { s.first_ts } else { e.first_ts.min(s.first_ts) };
                    }
                    e.online = e.online || s.online;
                    for (d, secs) in &s.days {
                        let v = e.days.entry(*d).or_insert(0);
                        *v = (*v).max(*secs);
                    }
                }
            }
        }
        let seeders: Vec<TrackerSeeder> = merged.into_values().collect();
        let est = estimate_app_accrual(&app.funding, &seeders, &reward, now);
        let est_sth = est as f64 / 1e8;
        let (forecast, next_payout) = forecast_current_day(&app.funding, &seeders, &reward, now);
        let forecast_sth = forecast as f64 / 1e8;
        let (my_online, online) = seeders
            .iter()
            .find(|s| s.address == reward)
            .map(|s| (s.days.values().sum::<u64>(), s.online))
            .unwrap_or((0, false));
        let bank_sth: u64 = app.funding.iter().filter(|f| f.ends_at > now).map(|f| f.amount_smarthoshi.parse::<u128>().unwrap_or(0)).sum::<u128>() as u64 / 100_000_000;
        let seconds_remaining = app.funding.iter().map(|f| f.ends_at).max().unwrap_or(now).saturating_sub(now);
        total_est += est_sth;
        active.push(DappEarning {
            id: id.clone(),
            name: app.name.clone(),
            bank_sth,
            daily_pool_sth: bank_sth as f64 / 5.0,
            seconds_remaining,
            my_online_secs: my_online,
            seeders: seeders.len(),
            online,
            est_sth,
            forecast_sth,
            next_payout_secs: next_payout,
        });
    }
    active.sort_by(|a, b| b.est_sth.partial_cmp(&a.est_sth).unwrap_or(std::cmp::Ordering::Equal));
    EarningsSummary { reward_address: reward, total_est_sth: total_est, active_apps: active }
}

// ============================================================
//  Phase G+ — история фактических выплат сидера (payout history)
//
//  Сканирует on-chain историю кошелька rewardAddress и извлекает входящие
//  transfers с memo `payout:<appId>` — фактические зачисления от payout-сервера.
//  Показывается в модалке "История выплат" на странице MyApplications. В отличие
//  от `network_earnings` (прогноз/оценка), это уже подтверждённые сетью выплаты.
// ============================================================

#[derive(Debug, Serialize, Clone)]
pub(crate) struct PayoutItem {
    pub id: String,
    /// Идентификатор dApp, за сидирование которого пришла выплата (memo tail).
    #[serde(rename = "appId")]
    pub app_id: String,
    /// Отображаемое имя dApp (заполняется trackers-каталогом, если найдено).
    #[serde(rename = "appName")]
    pub app_name: String,
    /// Сумма выплаты в STH (строка, форматированная как остальная история).
    #[serde(rename = "amountSth")]
    pub amount_sth: String,
    /// Unix ms — когда выплата пришла на кошелёк сидера.
    pub timestamp: u64,
    /// Адрес payout-сервера/эскроу-узла, приславшего средства.
    pub payer: String,
    /// Полное memo транзакции (`payout:<appId>[:extra]`) для аудита.
    pub memo: String,
    pub confirmed: bool,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct PayoutHistoryOut {
    pub items: Vec<PayoutItem>,
    /// Суммарно выплачено за всю просматриваемую историю (STH, 4 знака).
    #[serde(rename = "totalSth")]
    pub total_sth: String,
    /// Количество уникальных dApp'ов, за которые платили.
    pub apps: u32,
    /// Количество отдельных транзакций-выплат.
    pub count: u32,
}

#[tauri::command]
pub(crate) async fn payout_history(reward_address: String) -> PayoutHistoryOut {
    tauri::async_runtime::spawn_blocking(move || {
        let empty = || PayoutHistoryOut {
            items: Vec::new(),
            total_sth: "0.0000".into(),
            apps: 0,
            count: 0,
        };
        let addr = reward_address.trim().to_string();
        if addr.is_empty() {
            return empty();
        }
        // Индекс имён dApp'ов из локального каталога опубликованных приложений
        // (Sled `apps:` пространство): подставляем человекочитаемое имя в UI.
        let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for kv in crate::DB.scan_prefix(b"apps:").flatten() {
            if let Ok(v) = std::str::from_utf8(&kv.1) {
                if let Ok(m) = serde_json::from_str::<serde_json::Value>(v) {
                    if let (Some(id), Some(name)) = (m["id"].as_str(), m["name"].as_str()) {
                        names.insert(id.to_string(), name.to_string());
                    }
                }
            }
        }
        let items_raw = crate::sth_node::fetch_history_cached(&addr);
        let mut items: Vec<PayoutItem> = Vec::new();
        let mut total: f64 = 0.0;
        let mut app_set = std::collections::HashSet::new();
        // seed:reward мультиплатежи: нода не отдаёт их в истории кошелька
        // получателя (SappsP→SappsP), поэтому отдельный инкрементальный сбор
        // в локальную sled-базу + чтение оттуда (без пересканирования).
        // seed:reward: чтение МГНОВЕННО из sled; сеть — в фоне (троттл 2 мин
        // внутри sync_seed_rewards), результат подтянется при след. открытии
        {
            let a = addr.clone();
            std::thread::spawn(move || crate::sth_node::sync_seed_rewards(&a));
        }
        for (id, amt, ts) in crate::sth_node::seed_rewards_local(&addr) {
            total += amt as f64 / 100_000_000.0;
            app_set.insert("seed:reward".to_string());
            items.push(PayoutItem {
                id,
                app_id: "seed:reward".into(),
                app_name: "SEED REWARD".into(),
                amount_sth: crate::sth_node::smarthoshi_to_sth(amt),
                timestamp: ts,
                payer: crate::sth_node::SEED_PAYER.into(),
                memo: "seed:reward".into(),
                confirmed: true,
            });
        }
        for h in items_raw.into_iter() {
            if h.kind != "in" {
                continue;
            }
            // seed:reward из обычной истории пропускаем — уже учтён из sled
            if h.memo == "seed:reward" {
                continue;
            }
            if !h.memo.starts_with("payout:") {
                continue;
            }
            // memo может быть `payout:<appId>` или `payout:<appId>:<day>` — берём appId.
            let tail = &h.memo["payout:".len()..];
            let app_id = tail.split(':').next().unwrap_or("").to_string();
            if app_id.is_empty() {
                continue;
            }
            app_set.insert(app_id.clone());
            total += h.amount.parse::<f64>().unwrap_or(0.0);
            let name = names.get(&app_id).cloned().unwrap_or_else(|| app_id.clone());
            items.push(PayoutItem {
                id: h.id,
                app_id,
                app_name: name,
                amount_sth: h.amount,
                timestamp: h.timestamp,
                payer: h.counterparty,
                memo: h.memo,
                confirmed: h.confirmed,
            });
        }
        // Свежие выплаты сверху (лента как в кошельке).
        items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        PayoutHistoryOut {
            count: items.len() as u32,
            apps: app_set.len() as u32,
            total_sth: format!("{:.4}", total),
            items,
        }
    })
    .await
    .unwrap_or_else(|_| PayoutHistoryOut {
        items: Vec::new(),
        total_sth: "0.0000".into(),
        apps: 0,
        count: 0,
    })
}

#[tauri::command]
pub(crate) fn set_trackers(list: Vec<String>) -> Result<(), String> {
    let joined = list
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    DB.insert(b"cfg:trackers", joined.into_bytes())
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// SmartNet DHT bootstrap seeders (ip:port). Resolution order:
///   1. user list in sled (`cfg:seeders`, set via Network Settings)
///   2. auto-discovered seeders from trackers (`cfg:seeders_auto`, GET /seeders-dht)
///   3. `SMARTNET_SEEDERS` env (comma/space/newline separated) — deploy default
///   4. hardcoded [`DEFAULT_SEEDERS`] fallback
/// Multiple seeders are supported so the network isn't tied to a single host.
/// Used by the DHT build paths (union of all sources, deduped).
pub(crate) fn get_seeders_internal() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let read = |key: &[u8], out: &mut Vec<String>| {
        if let Ok(Some(v)) = DB.get(key) {
            if let Ok(s) = String::from_utf8(v.to_vec()) {
                out.extend(s.lines().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
            }
        }
    };
    read(b"cfg:seeders", &mut out); // manual (Settings)
    read(b"cfg:seeders_auto", &mut out); // auto-discovered (trackers)
    if let Ok(env) = std::env::var("SMARTNET_SEEDERS") {
        out.extend(
            env.split(|c| c == ',' || c == ' ' || c == '\n')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty()),
        );
    }
    out.extend(DEFAULT_SEEDERS.iter().map(|s| s.to_string()));
    out.sort();
    out.dedup();
    out
}

/// Full effective seeder set (manual + auto + env + default) for highlighting on
/// the Network Map globe.
#[tauri::command]
pub(crate) fn get_seeders_effective() -> Vec<String> {
    get_seeders_internal()
}

/// Manual seeder list only (for the Settings textarea — excludes auto/env/default).
#[tauri::command]
pub(crate) fn get_seeders() -> Vec<String> {
    if let Ok(Some(v)) = DB.get(b"cfg:seeders") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            return s.lines().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        }
    }
    Vec::new()
}

#[tauri::command]
pub(crate) fn set_seeders(list: Vec<String>) -> Result<(), String> {
    let joined = list
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && s.parse::<std::net::SocketAddr>().is_ok())
        .collect::<Vec<_>>()
        .join("\n");
    DB.insert(b"cfg:seeders", joined.into_bytes())
        .map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Auto-discovered seeders (from trackers' /seeders-dht) — read-only, for the
/// Settings badge "+N auto-discovered".
#[tauri::command]
pub(crate) fn get_seeders_auto() -> Vec<String> {
    if let Ok(Some(v)) = DB.get(b"cfg:seeders_auto") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            return s.lines().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        }
    }
    Vec::new()
}

/// Persist auto-discovered seeders (from trackers' GET /seeders-dht), validated + capped.
pub(crate) fn save_seeders_auto(list: &[String]) {
    let joined = list
        .iter()
        .filter(|s| s.parse::<std::net::SocketAddr>().is_ok())
        .take(32)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    if !joined.is_empty() {
        let _ = DB.insert(b"cfg:seeders_auto", joined.into_bytes());
        let _ = DB.flush();
    }
}

#[derive(serde::Deserialize)]
struct SeedersDhtResp {
    #[serde(default)]
    seeders: Vec<String>,
    #[serde(default)]
    sig: String,
}

/// Resolved operator pubkey for /seeders-dht verification:
///   env `SMARTNET_OPERATOR_PUBKEY` (hard override) → pinned `cfg:operator_pubkey`
///   (TOFU, auto-pinned from a tracker) → None (unsigned mode).
pub(crate) fn operator_pubkey() -> Option<String> {
    if let Ok(p) = std::env::var("SMARTNET_OPERATOR_PUBKEY") {
        let p = p.trim().to_string();
        if !p.is_empty() {
            return Some(p);
        }
    }
    if let Ok(Some(v)) = DB.get(b"cfg:operator_pubkey") {
        if let Ok(s) = String::from_utf8(v.to_vec()) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Trust-on-first-use: pin the operator pubkey advertised by a tracker (via
/// /operator-pubkey or /stats.pubkey). Only pins if nothing is configured yet and
/// the key is a valid secp256k1 pubkey — old trackers omit it ⇒ unsigned mode.
fn pin_operator_pubkey(hex_key: &str) {
    let k = hex_key.trim();
    if k.is_empty() || operator_pubkey().is_some() {
        return;
    }
    if hex::decode(k).ok().and_then(|b| secp256k1::PublicKey::from_slice(&b).ok()).is_some() {
        let _ = DB.insert(b"cfg:operator_pubkey", k.as_bytes());
        let _ = DB.flush();
        crate::syslog::log("net", format!("pinned operator pubkey (TOFU): {}…", &k[..k.len().min(16)]));
    }
}

/// Verify the operator signature over a tracker's seeder list, IF an operator
/// pubkey is known (env override or TOFU-pinned):
///   • no pubkey → true (unsigned mode — backward compatible with old trackers)
///   • pubkey + sig valid over sha256(seeders.join("\n")) → true
///   • pubkey + sig missing/invalid → false (reject poisoned/MITM'd list)
/// Общий верификатор списков с трекера (seeders-dht, pqweb): канон = lines.join("\n").
pub(crate) fn operator_seeders_ok(seeders: &[String], sig_hex: &str) -> bool {
    let pub_hex = match operator_pubkey() {
        Some(p) => p,
        None => return true,
    };
    let pubkey = match hex::decode(pub_hex).ok().and_then(|b| secp256k1::PublicKey::from_slice(&b).ok()) {
        Some(k) => k,
        None => return false,
    };
    let sig = match hex::decode(sig_hex.trim()).ok().and_then(|b| secp256k1::ecdsa::Signature::from_compact(&b).ok()) {
        Some(s) => s,
        None => return false,
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(seeders.join("\n").as_bytes());
    match secp256k1::Message::from_digest_slice(&digest) {
        Ok(msg) => secp256k1::Secp256k1::verification_only()
            .verify_ecdsa(&msg, &sig, &pubkey)
            .is_ok(),
        Err(_) => false,
    }
}


pub(crate) fn relay_only_internal() -> bool {
    matches!(DB.get(b"cfg:relay_only"), Ok(Some(v)) if v.as_ref() == b"1")
}

#[tauri::command]
pub(crate) fn get_relay_only() -> bool {
    relay_only_internal()
}

#[tauri::command]
pub(crate) fn set_relay_only(enabled: bool) -> Result<(), String> {
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:relay_only", val).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Мобильные фолбэки (Phase 2, обход операторских фильтров): режим хранится в
/// Sled `cfg:mobile_fb`. "auto" (по умолчанию) — включаются сами, когда
/// системный DNS не отвечает; "on" — принудительно; "off" — никогда.
pub(crate) fn mobile_fallbacks_mode() -> String {
    match DB.get(b"cfg:mobile_fb") {
        Ok(Some(v)) => String::from_utf8_lossy(&v).to_string(),
        _ => "auto".to_string(),
    }
}

#[tauri::command]
pub(crate) fn get_mobile_fallbacks() -> String {
    mobile_fallbacks_mode()
}

#[tauri::command]
pub(crate) fn set_mobile_fallbacks(mode: String) -> Result<(), String> {
    if !matches!(mode.as_str(), "auto" | "on" | "off") {
        return Err("режим должен быть auto|on|off".to_string());
    }
    DB.insert(b"cfg:mobile_fb", mode.as_bytes()).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Swarm-загрузка dApp: bandit-failover при установке/пре-фетче. По умолчанию
/// ВКЛЮЧЕНО (при любой ошибке swarm — безопасный откат на прежнюю логику, так
/// что установка не ломается). Хранится в Sled `cfg:swarm_enabled`.
pub(crate) fn swarm_enabled_internal() -> bool {
    !matches!(DB.get(b"cfg:swarm_enabled"), Ok(Some(v)) if v.as_ref() == b"0")
}

#[tauri::command]
pub(crate) fn get_swarm_enabled() -> bool {
    swarm_enabled_internal()
}

#[tauri::command]
pub(crate) fn set_swarm_enabled(enabled: bool) -> Result<(), String> {
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:swarm_enabled", val).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Один сидер в рейтинге swarm-бандита (для виджета «топ сидеров»).
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwarmSeeder {
    /// EndpointId пира.
    pub peer: String,
    /// Оценка скорости (номинальные байт/сек; выше = быстрее).
    pub speed: f64,
    /// Подряд идущие ошибки на момент снимка.
    pub errors: u32,
    pub attempts: u32,
    pub successes: u32,
}

/// Топ-N сидеров по скорости из весов бандита (пусто без фичи p2p).
#[tauri::command(async)]
pub(crate) fn swarm_top_seeders() -> Vec<SwarmSeeder> {
    #[cfg(feature = "p2p")]
    {
        p2p_impl::top_seeders(12)
    }
    #[cfg(not(feature = "p2p"))]
    {
        Vec::new()
    }
}

/// Трекерлесс-обнаружение сидеров dApp через Mainline DHT + identity-beacon.
/// По умолчанию ВКЛ (аддитивно к трекерам/mDNS; при недоступном DHT — фолбэк).
/// Хранится в Sled `cfg:dht_providers_enabled`.
pub(crate) fn dht_providers_enabled_internal() -> bool {
    !matches!(DB.get(b"cfg:dht_providers_enabled"), Ok(Some(v)) if v.as_ref() == b"0")
}

#[tauri::command]
pub(crate) fn get_dht_providers_enabled() -> bool {
    dht_providers_enabled_internal()
}

#[tauri::command]
pub(crate) fn set_dht_providers_enabled(enabled: bool) -> Result<(), String> {
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:dht_providers_enabled", val).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Сводка трекерлесс-обнаружения для индикатора на Dashboard и панели в Settings.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DhtProvidersSummary {
    /// Включён ли трекерлесс-режим.
    pub enabled: bool,
    /// Уникальных сидеров, найденных через DHT (по кэшу всех приложений).
    pub seeders: u32,
    /// Для скольких dApp есть DHT-найденные провайдеры.
    pub apps: u32,
    /// Сколько dApp мы сейчас анонсируем в DHT (served-набор бикона).
    pub announced_apps: u32,
    /// UDP-порт поднятого identity-beacon (None — ещё не поднят).
    pub beacon_port: Option<u16>,
    /// Узлов в таблице маршрутизации Mainline DHT (0 — DHT ещё не готов).
    pub dht_nodes: u64,
}

#[tauri::command(async)]
pub(crate) fn dht_providers_summary() -> DhtProvidersSummary {
    let enabled = dht_providers_enabled_internal();
    let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut apps = 0u32;
    for kv in DB.scan_prefix(b"dht:providers:").flatten() {
        if let Ok(s) = std::str::from_utf8(&kv.1) {
            let mut any = false;
            for p in s.split(',') {
                let p = p.trim();
                if !p.is_empty() {
                    set.insert(p.to_string());
                    any = true;
                }
            }
            if any {
                apps += 1;
            }
        }
    }
    #[cfg(feature = "p2p")]
    let (beacon_port, announced_apps) = p2p_impl::beacon_diag();
    #[cfg(not(feature = "p2p"))]
    let (beacon_port, announced_apps): (Option<u16>, u32) = (None, 0);
    DhtProvidersSummary {
        enabled,
        seeders: set.len() as u32,
        apps,
        announced_apps,
        beacon_port,
        dht_nodes: crate::torrent::get_torrent_dht_nodes(),
    }
}

/// Провайдер прямой установки dApp: нода-раздатчик + хэш tar-блоба.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectProvider {
    pub node_id: String,
    pub hash: String,
}

/// Поиск данных прямой ссылки: hash из SEEDED_APPS (полный набор раздач,
/// включая бесплатные «только по ссылке» dApp — TRACKER_SNAPSHOT содержит лишь
/// анонсируемые платные приложения, из-за чего прямая ссылка для free падала
/// «not-seeded»), nodeId — своя нода; fallback — снапшот трекера.
fn share_info_lookup(id: &str) -> Option<DirectProvider> {
    let hash = SEEDED_APPS.lock().unwrap().get(id).cloned().unwrap_or_default();
    let node_id = {
        let own = OWN_IDENTITY.lock().unwrap();
        own.1.clone()
    };
    if !hash.is_empty() && !node_id.is_empty() {
        return Some(DirectProvider { node_id, hash });
    }
    let snap = TRACKER_SNAPSHOT.lock().unwrap().clone()?;
    let hash = snap
        .apps
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.hash.clone())
        .unwrap_or_default();
    if hash.is_empty() || snap.endpoint_id.is_empty() {
        return None;
    }
    Some(DirectProvider { node_id: snap.endpoint_id, hash })
}

/// Данные для «прямой ссылки» на СВОЁ опубликованное приложение:
/// `sth://<id>?n=<nodeId>&h=<hash>` — получатель ставит dApp напрямую с ноды
/// издателя, без трекеров и headless-сидеров (нода должна быть онлайн).
/// Только что опубликованный dApp: фоновый seed_all после finalize мог ещё не
/// добежать — пересидируем сами (await) и повторяем поиск, чтобы кнопка
/// DIRECT LINK / QR работала сразу после публикации, а не «NOT SEEDING YET».
#[tauri::command]
pub(crate) async fn app_share_info(
    app: tauri::AppHandle,
    id: String,
) -> Result<DirectProvider, String> {
    if let Some(dp) = share_info_lookup(&id) {
        return Ok(dp);
    }
    let _ = p2p_impl::seed_all(&app).await;
    share_info_lookup(&id).ok_or_else(|| "not-seeded".to_string())
}

/// Результат трекерлесс-резолва dApp через Mainline DHT + identity-биконы.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DhtResolveResult {
    pub providers: Vec<String>,
    pub hash: String,
    pub seeders: u32,
}

/// Трекерлесс-резолв НЕИЗВЕСТНОГО dApp по id: BEP5 get_peers(app_infohash) +
/// LAN-broadcast → опрос identity-биконов → проверенные EndpointId-провайдеры
/// и хэш tar-блоба из v2-ответа. Этого достаточно для установки БЕЗ трекеров.
#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn dht_resolve_app(id: String) -> Result<DhtResolveResult, String> {
    let ih = crate::dht_dapp::app_infohash(&id);
    let cache_key = format!("dht:providers:{}", id);
    let (providers, xh, seeders) = p2p_impl::discover_extra(ih, &cache_key).await;
    let hash = xh.unwrap_or_default();
    if providers.is_empty() || hash.is_empty() {
        return Err("not-found".to_string());
    }
    Ok(DhtResolveResult { providers, hash, seeders })
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn dht_resolve_app(_id: String) -> Result<DhtResolveResult, String> {
    Err("p2p-disabled".to_string())
}

/// Mainline DHT (pkarr) discovery — бессерверное обнаружение NodeID через сеть
/// BitTorrent. По умолчанию ВКЛЮЧЕНО (релеи остаются транспортным fallback).
/// Применяется при следующем запуске узла (после рестарта приложения).
pub(crate) static DHT_NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Снимок IP:port живых узлов из таблицы маршрутизации DHT (для глобуса карты сети).
pub(crate) static DHT_PEERS: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub(crate) fn dht_enabled_internal() -> bool {
    !matches!(DB.get(b"cfg:dht_enabled"), Ok(Some(v)) if v.as_ref() == b"0")
}

#[tauri::command]
pub(crate) fn get_dht_enabled() -> bool {
    dht_enabled_internal()
}

#[tauri::command]
pub(crate) fn set_dht_enabled(enabled: bool) -> Result<(), String> {
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:dht_enabled", val).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct DhtStats {
    pub enabled: bool,
    pub nodes: u64,
    /// Поднялся ли DHT через SmartNet-сидер (его IP виден в таблице) vs публичный.
    pub via_smartnet: bool,
}

#[tauri::command]
pub(crate) fn dht_stats() -> DhtStats {
    // SmartNet-сидер виден в таблице маршрутизации → DHT поднят через нашу
    // инфраструктуру (а не только через публичные BT-роутеры).
    const SMARTNET_DHT_IP: &str = "116.202.32.250";
    let via_smartnet = DHT_PEERS
        .lock()
        .unwrap()
        .iter()
        .any(|a| a.starts_with(SMARTNET_DHT_IP));
    DhtStats {
        enabled: dht_enabled_internal(),
        nodes: DHT_NODES.load(std::sync::atomic::Ordering::Relaxed),
        via_smartnet,
    }
}

/// Снимок IP:port живых DHT-узлов (для отображения точками на глобусе карты сети).
#[tauri::command(async)]
pub(crate) fn dht_nodes_list() -> Vec<String> {
    DHT_PEERS.lock().unwrap().clone()
}

/// Сколько живых DHT-узлов максимум кэшируем для extra_bootstrap.
const DHT_BOOT_MAX: usize = 64;

/// Прочитать кэш bootstrap-узлов DHT из sled (cfg:dht_boot, по строке на узел).
pub(crate) fn load_dht_bootstrap() -> Vec<String> {
    DB.get(b"cfg:dht_boot")
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .map(|s| {
            s.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .take(DHT_BOOT_MAX)
                .collect()
        })
        .unwrap_or_default()
}

/// Сохранить живые DHT-узлы из таблицы маршрутизации в кэш (для след. старта).
pub(crate) fn save_dht_bootstrap(nodes: &[String]) {
    let body = nodes.iter().take(DHT_BOOT_MAX).cloned().collect::<Vec<_>>().join("\n");
    let _ = DB.insert(b"cfg:dht_boot", body.into_bytes());
    let _ = DB.flush();
}

/// Прочитать последний известный размер таблицы маршрутизации DHT из sled.
/// Используется для МГНОВЕННОГО показа кэшированного значения при открытии
/// Карты сети / Dashboard, пока свежий DHT-клиент бутстрапится (~30-60с) —
/// вместо мигающего 0.
pub(crate) fn load_dht_nodes_count() -> u64 {
    DB.get(b"cfg:dht_nodes")
        .ok()
        .flatten()
        .and_then(|v| String::from_utf8(v.to_vec()).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Сохранить последний известный размер таблицы маршрутизации DHT в sled.
pub(crate) fn save_dht_nodes_count(n: u64) {
    let _ = DB.insert(b"cfg:dht_nodes", n.to_string().into_bytes());
    let _ = DB.flush();
}

/// Anti-spam throttle: earliest unix-ms at which we'll accept the next batch of
/// peer-supplied DHT hints (rate-limits flooding from the tracker/gossip).
pub(crate) static DHT_HINT_NEXT_OK: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
const DHT_HINT_MIN_INTERVAL_MS: u64 = 10_000;
const DHT_HINT_MAX_PER_BATCH: usize = 8;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A small (≤8) sample of our OWN live DHT nodes to share with peers.
pub(crate) fn dht_share_sample() -> Vec<String> {
    DHT_PEERS
        .lock()
        .unwrap()
        .iter()
        .take(DHT_HINT_MAX_PER_BATCH)
        .cloned()
        .collect()
}

/// Merge peer-supplied DHT bootstrap hints (ip:port) into the local cache.
/// Hardened against spam/poisoning: rate-limited, count-capped, each entry must
/// parse as a real socket addr, deduped, and the whole cache stays ≤ DHT_BOOT_MAX.
/// Returns true if at least one NEW node was added (signals a possible re-bootstrap).
pub(crate) fn merge_dht_hints(hints: Vec<String>) -> bool {
    if hints.is_empty() {
        return false;
    }
    // anti-flood: ignore batches that arrive faster than the min interval
    let now = now_ms();
    let next_ok = DHT_HINT_NEXT_OK.load(std::sync::atomic::Ordering::Relaxed);
    if now < next_ok {
        return false;
    }
    DHT_HINT_NEXT_OK.store(now + DHT_HINT_MIN_INTERVAL_MS, std::sync::atomic::Ordering::Relaxed);

    let mut current = load_dht_bootstrap();
    let existing: std::collections::HashSet<String> = current.iter().cloned().collect();
    let mut added = false;
    for raw in hints.into_iter().take(DHT_HINT_MAX_PER_BATCH) {
        let n = raw.trim().to_string();
        if n.parse::<std::net::SocketAddr>().is_err() || existing.contains(&n) {
            continue;
        }
        current.push(n);
        added = true;
        if current.len() >= DHT_BOOT_MAX {
            break;
        }
    }
    if added {
        save_dht_bootstrap(&current);
        crate::syslog::log("dht", format!("merged peer DHT hints → {} cached bootstrap nodes", current.len()));
    }
    added
}

/// SmartNet custom relays (n1-relay). Opt-in: when enabled, the listed relay
/// URLs are added to the iroh endpoint's relay map alongside the default n0
/// relays, so users can route through operator-hosted SmartNet relays.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct RelayConfigOut {
    pub enabled: bool,
    pub urls: Vec<String>,
}

#[tauri::command]
pub(crate) fn get_relay_config() -> RelayConfigOut {
    let enabled = matches!(DB.get(b"cfg:n1_relay_enabled"), Ok(Some(v)) if v.as_ref() == b"1");
    let urls = DB
        .get(b"cfg:n1_relay_urls")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice::<Vec<String>>(&v).ok())
        .unwrap_or_default();
    RelayConfigOut { enabled, urls }
}

#[tauri::command]
pub(crate) fn set_relay_config(enabled: bool, urls: Vec<String>) -> Result<RelayConfigOut, String> {
    let cleaned: Vec<String> = urls
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:n1_relay_enabled", val).map_err(|e| e.to_string())?;
    let j = serde_json::to_vec(&cleaned).map_err(|e| e.to_string())?;
    DB.insert(b"cfg:n1_relay_urls", j).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(RelayConfigOut { enabled, urls: cleaned })
}

/// SmartNet custom relays (n1-relay): the apply helper lives inside `p2p_impl`
/// (below) where the iroh `Endpoint` type is in scope.

/// Встроенные SmartNet-реле (открытый TCP/TLS 443, валидный сертификат,
/// A + AAAA записи — досягаемы и по IPv4, и по IPv6). ВСЕГДА добавляются в
/// relay map endpoint'а (кроме стелс-режима): при полностью зарезанном UDP
/// трафик автоматически идёт через них по WSS/TLS-443, как обычный HTTPS.
pub(crate) const DEFAULT_SMARTNET_RELAYS: &[&str] =
    &["https://relay-fsn7.sth.cx", "https://relay-ru1.sth.cx"];

/// Эффективный список SmartNet-реле: встроенные дефолты + пользовательские
/// n1-реле (если тумблер включён), без дубликатов.
pub(crate) fn effective_relay_urls() -> Vec<String> {
    let mut v: Vec<String> = DEFAULT_SMARTNET_RELAYS.iter().map(|s| s.to_string()).collect();
    let cfg = get_relay_config();
    if cfg.enabled {
        for u in cfg.urls {
            let u = u.trim_end_matches('/').to_string();
            if !v.contains(&u) {
                v.push(u);
            }
        }
    }
    v
}


/// Auto-update preference (default ON). When ON, the client silently fetches +
/// applies published dApp updates it discovers. When OFF, it pre-fetches the
/// new blobs in the background but waits for explicit user confirmation to apply.
pub(crate) fn auto_update_internal() -> bool {
    // default ON unless explicitly disabled ("0")
    !matches!(DB.get(b"cfg:auto_update"), Ok(Some(v)) if v.as_ref() == b"0")
}

#[tauri::command]
pub(crate) fn get_auto_update() -> bool {
    auto_update_internal()
}

#[tauri::command]
pub(crate) fn set_auto_update(enabled: bool) -> Result<(), String> {
    let val: &[u8] = if enabled { b"1" } else { b"0" };
    DB.insert(b"cfg:auto_update", val).map_err(|e| e.to_string())?;
    let _ = DB.flush();
    Ok(())
}

/// Build the heartbeat payload from local manifests + freshly-seeded blob hashes.
pub(crate) fn build_tracker_snapshot(
    node: &str,
    endpoint_id: &str,
    seeded: &HashMap<String, String>,
) -> TrackerSnapshot {
    let mut apps = Vec::new();
    for id in crate::announced_apps_internal() {
        if let Ok(Some(v)) = DB.get(format!("{}{}", APP_PREFIX, id).as_bytes()) {
            if let Ok(m) = serde_json::from_slice::<Manifest>(&v) {
                let size: u64 = m.files.iter().map(|f| f.size).sum();
                // Приватный dApp: description несёт reveal-payload (§P1§…) —
                // клиенты из списка доступа расшифруют имя/описание, остальные скроют.
                let description = if m.private == Some(true) {
                    crate::private_dapp::reveal_payload_for(&id).unwrap_or_default()
                } else {
                    m.description.unwrap_or_default()
                };
                apps.push(TrackerAnnounceApp {
                    id: m.id,
                    name: m.name,
                    description,
                    tags: m.tags,
                    merkle: m.merkle,
                    version: m.version.unwrap_or(1),
                    hash: seeded.get(&id).cloned().unwrap_or_default(),
                    updated: m.updated.unwrap_or_default(),
                    size,
                    signature: m.signature,
                    public_key: m.public_key,
                    owner: m.owner,
                    logo: SEEDED_LOGOS.lock().unwrap().get(&id).cloned().unwrap_or_default(),
                    preview: SEEDED_PREVIEWS.lock().unwrap().get(&id).cloned().unwrap_or_default(),
                });
            }
        }
    }
    TrackerSnapshot {
        node: node.to_string(),
        endpoint_id: endpoint_id.to_string(),
        apps,
    }
}

/// Merge a tracker's directory into PEERS (keyed by `tracker:<endpointId>` so
/// WAN providers coexist with the LAN mDNS peers without clobbering them).
pub(crate) fn merge_tracker_apps(apps: Vec<TrackerApp>) {
    let mut by_endpoint: HashMap<String, PeerInfo> = HashMap::new();
    for a in apps {
        for p in a.providers {
            // трекер возвращает и наш собственный announce — фильтруем себя
            if is_self_peer(&p.node, &p.endpoint_id) {
                continue;
            }
            let entry = by_endpoint
                .entry(p.endpoint_id.clone())
                .or_insert_with(|| PeerInfo {
                    node: p.node.clone(),
                    node_id: p.endpoint_id.clone(),
                    addr: String::new(),
                    apps: Vec::new(),
                    last_seen: now_iso(),
                    via_seeder: false,
                });
            entry.apps.push(PeerApp {
                id: a.id.clone(),
                hash: p.hash.clone(),
                title: a.name.clone(),
                description: a.description.clone(),
                tags: a.tags.clone(),
                merkle: a.merkle.clone(),
                // prefer the provider's own served version; fall back to the
                // app-level version the tracker reports.
                version: if p.version > 0 { p.version } else { a.version },
                updated: a.updated.clone(),
                paid: a.paid,
                boost_pool: a.boost_pool,
                priority_weight: a.priority_weight,
                size_bytes: a.size_bytes,
                cost: a.cost,
                owner: a.owner.clone(),
                logo: a.logo.clone(),
                preview: a.preview.clone(),
            });
        }
    }
    let mut peers = PEERS.lock().unwrap();
    for (eid, info) in by_endpoint {
        peers.insert(format!("tracker:{}", eid), info);
    }
}

/// One heartbeat + fetch cycle against all configured trackers (blocking).
pub(crate) fn tracker_tick() {
    let trackers = get_trackers_internal();
    let to = std::time::Duration::from_secs(5);

    // 1. announce the apps we serve (needs an iroh endpoint + at least one app)
    let snap = TRACKER_SNAPSHOT.lock().unwrap().clone();
    if let Some(s) = snap {
        if !s.endpoint_id.is_empty() && !s.apps.is_empty() {
            let body = TrackerAnnounceReq {
                node: s.node,
                endpoint_id: s.endpoint_id,
                apps: s.apps,
                // share a few of our live DHT nodes for censored peers to bootstrap off
                dht: dht_share_sample(),
            };
            if let Ok(payload) = serde_json::to_string(&body) {
                for t in &trackers {
                    let url = format!("{}/announce", normalize_tracker(t));
                    let _ = ureq::post(&url)
                        .timeout(to)
                        .set("Content-Type", "application/json")
                        .send_string(&payload);
                }
            }
        }
    }

    // 1b. Anti-censorship bootstrap: if our DHT is weak/empty, harvest the
    // tracker's pooled DHT nodes (GET /dht) and cache them as extra_bootstrap.
    // (Hardened by merge_dht_hints: rate-limited, validated, capped.)
    if DHT_NODES.load(std::sync::atomic::Ordering::Relaxed) < 8 {
        for t in &trackers {
            let url = format!("{}/dht", normalize_tracker(t));
            if let Ok(resp) = ureq::get(&url).timeout(to).call() {
                if let Ok(txt) = resp.into_string() {
                    if let Ok(nodes) = serde_json::from_str::<Vec<String>>(&txt) {
                        merge_dht_hints(nodes);
                    }
                }
            }
        }
    }

    // 1c. Auto-discover working SmartNet seeders (GET /seeders-dht) so users
    // never have to add seeders manually — full decentralized discovery. The
    // pooled set is cached and folded into the DHT bootstrap on (re)build.
    {
        let mut discovered: Vec<String> = Vec::new();
        for t in &trackers {
            // TOFU: pin the operator pubkey advertised by the tracker (if any &
            // none configured yet) so /seeders-dht can be verified — no env needed.
            // Old trackers omit it ⇒ stays unsigned.
            if operator_pubkey().is_none() {
                let pk_url = format!("{}/operator-pubkey", normalize_tracker(t));
                if let Ok(resp) = ureq::get(&pk_url).timeout(to).call() {
                    if let Ok(txt) = resp.into_string() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                            if let Some(pk) = v.get("pubkey").and_then(|x| x.as_str()) {
                                pin_operator_pubkey(pk);
                            }
                        }
                    }
                }
            }
            let url = format!("{}/seeders-dht", normalize_tracker(t));
            if let Ok(resp) = ureq::get(&url).timeout(to).call() {
                if let Ok(txt) = resp.into_string() {
                    if let Ok(r) = serde_json::from_str::<SeedersDhtResp>(&txt) {
                        // Reject poisoned/MITM'd lists when an operator key is known.
                        if !r.seeders.is_empty() && operator_seeders_ok(&r.seeders, &r.sig) {
                            discovered.extend(r.seeders);
                        }
                    }
                }
            }
        }
        if !discovered.is_empty() {
            discovered.sort();
            discovered.dedup();
            save_seeders_auto(&discovered);
        }
    }

    // 2. fetch the directory from each tracker and merge providers into PEERS
    for t in &trackers {
        let url = format!("{}/apps", normalize_tracker(t));
        if let Ok(resp) = ureq::get(&url).timeout(to).call() {
            if let Ok(txt) = resp.into_string() {
                if let Ok(apps) = serde_json::from_str::<Vec<TrackerApp>>(&txt) {
                    merge_tracker_apps(apps);
                }
            }
        }
    }
}

pub(crate) fn start_tracker_thread() {
    let mut started = TRACKER_THREAD.lock().unwrap();
    if *started {
        return;
    }
    *started = true;
    let _ = std::thread::Builder::new().name("p2p-registry-loop".into()).spawn(|| loop {
        if crate::netwatch::is_online() {
            tracker_tick();
        }
        // 60 с (было 30): TTL трекера 120 с — запас в один пропуск, вдвое меньше нагрузки и шума
        std::thread::sleep(std::time::Duration::from_secs(60));
    });
}

// ============================================================
//  Network Map — aggregate the seeder leaderboard from all trackers.
//  Queries each tracker's GET /seeders, dedupes by endpointId (keeping the
//  highest Byte-Hours score seen) and returns a ranked list for the client's
//  native "Network" tab.
// ============================================================

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct NetworkSeeder {
    #[serde(rename = "endpointId")]
    endpoint_id: String,
    #[serde(default)]
    node: String,
    #[serde(rename = "metricsUrl", default)]
    metrics_url: String,
    #[serde(rename = "appsSeeded", default)]
    apps_seeded: usize,
    #[serde(rename = "gbSeeded", default)]
    gb_seeded: f64,
    #[serde(rename = "uptimeSecs", default)]
    uptime_secs: u64,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    online: bool,
    #[serde(rename = "lastSeen", default)]
    last_seen: u64,
}

/// endpointId-ы пиров, узнанных по локальной сети (mDNS): ключ в PEERS без
/// префикса "tracker:" (трекерные кладутся как "tracker:<eid>").
fn lan_peer_ids() -> std::collections::HashSet<String> {
    let peers = PEERS.lock().unwrap();
    peers
        .iter()
        .filter(|(k, _)| !k.starts_with("tracker:"))
        .filter_map(|(_, v)| {
            let id = v.node_id.trim();
            if id.is_empty() {
                None
            } else {
                Some(id.to_string())
            }
        })
        .collect()
}

/// Источник/транспорт обнаружения для каждого пира (честно, по данным iroh):
///   "direct" | "relay" — транспорт ЖИВОГО соединения (Endpoint::remote_info);
///   "mdns"            — пир известен из локальной сети (mDNS);
///   "wan"             — пир из директории трекера (по умолчанию).
/// DHT не атрибутируется per-peer (iroh 1.0 не отдаёт источник адреса по пиру);
/// автономность DHT показывается общим счётчиком узлов (dht_stats).
#[tauri::command]
pub(crate) async fn peer_sources(ids: Vec<String>) -> std::collections::HashMap<String, String> {
    let lan = lan_peer_ids();
    let transports = p2p_impl::peer_transports(ids.clone()).await;
    let mut out: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for id in ids {
        if let Some(tp) = transports.get(&id) {
            out.insert(id, tp.clone());
        } else if lan.contains(&id) {
            out.insert(id, "mdns".to_string());
        } else {
            out.insert(id, "wan".to_string());
        }
    }
    out
}


#[tauri::command]
pub(crate) async fn network_map() -> Vec<NetworkSeeder> {
    // Run the blocking HTTP aggregation off the UI thread so polling slow /
    // unreachable trackers never freezes the frontend.
    tauri::async_runtime::spawn_blocking(network_map_blocking)
        .await
        .unwrap_or_default()
}

/// Таймаут одного HTTP-запроса к трекеру в агрегаторах карты сети.
const TRACKER_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// v1.135.185: параллельный GET `<tracker><path>` ко всем трекерам (scoped
/// threads, таймаут 3 с) — ответ за ~3 с даже если половина трекеров мертва.
/// Раньше опрос шёл последовательно (4 × 5 с) и «ОБНОВЛЕНИЕ…» висело до 20 с.
fn poll_trackers_parallel<T: serde::de::DeserializeOwned + Send>(path: &str) -> Vec<T> {
    let trackers = get_trackers_internal();
    std::thread::scope(|sc| {
        let handles: Vec<_> = trackers
            .iter()
            .map(|t| {
                let url = format!("{}{}", normalize_tracker(t), path);
                sc.spawn(move || {
                    let resp = ureq::get(&url).timeout(TRACKER_POLL_TIMEOUT).call().ok()?;
                    let txt = resp.into_string().ok()?;
                    serde_json::from_str::<T>(&txt).ok()
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    })
}

fn network_map_blocking() -> Vec<NetworkSeeder> {
    // Dedupe by reward node address (fall back to endpointId) so a seeder that
    // restarted with a fresh ephemeral endpointId doesn't show as duplicates.
    let mut by_key: HashMap<String, NetworkSeeder> = HashMap::new();
    for list in poll_trackers_parallel::<Vec<NetworkSeeder>>("/seeders") {
        for s in list {
            let key = if s.node.trim().is_empty() {
                s.endpoint_id.clone()
            } else {
                s.node.trim().to_string()
            };
            let better = match by_key.get(&key) {
                None => true,
                Some(x) => {
                    if s.online != x.online {
                        s.online
                    } else if s.score != x.score {
                        s.score > x.score
                    } else {
                        s.last_seen > x.last_seen
                    }
                }
            };
            if better {
                by_key.insert(key, s);
            }
        }
    }
    let mut out: Vec<NetworkSeeder> = by_key.into_values().collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Global mesh storage capacity/usage aggregated by the trackers (Phase: Global
/// Cloud Capacity). Mirrors the tracker's `/network/storage` payload.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub(crate) struct NetworkStorage {
    #[serde(rename = "totalCapacityGb", default)]
    total_capacity_gb: f64,
    #[serde(rename = "totalUsedGb", default)]
    total_used_gb: f64,
    #[serde(rename = "totalFreeGb", default)]
    total_free_gb: f64,
    #[serde(rename = "onlineNodes", default)]
    online_nodes: usize,
    #[serde(rename = "totalNodes", default)]
    total_nodes: usize,
    #[serde(default)]
    history: Vec<NetHistPoint>,
    /// L2 reward pool balance (escrow bank) in whole STH, live from the chain.
    #[serde(rename = "paymentPoolBalanceSth", default)]
    payment_pool_balance_sth: f64,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub(crate) struct NetHistPoint {
    #[serde(default)]
    t: u64,
    #[serde(rename = "usedGb", default)]
    used_gb: f64,
}

#[tauri::command]
pub(crate) async fn network_storage() -> NetworkStorage {
    tauri::async_runtime::spawn_blocking(network_storage_blocking)
        .await
        .unwrap_or_default()
}

fn network_storage_blocking() -> NetworkStorage {
    // Query every tracker (parallel) and keep the most complete view (the one
    // reporting the largest total capacity — i.e. the tracker that sees the most seeders).
    let mut best = NetworkStorage::default();
    for ns in poll_trackers_parallel::<NetworkStorage>("/network/storage") {
        if ns.total_capacity_gb >= best.total_capacity_gb {
            best = ns;
        }
    }
    // Live L2 reward-pool balance (escrow bank) — pulled fresh, never hardcoded.
    if let Ok((bal, _)) = crate::sth_node::fetch_balance_nonce(crate::oracle::ESCROW_ADDRESS) {
        best.payment_pool_balance_sth = (bal as f64) / 1e8;
    }
    best
}


// ============================================================
//  iroh-blobs transit (OPTIONAL — `p2p` feature). Real cross-PC dApp sync:
//  publish/discovery seeds each app folder into a local blob store and
//  announces (app_id : collection-hash); install fetches the collection from
//  a serving node and exports it to data/dapps/<id>, then verify_app proves
//  integrity. When the `p2p` feature is off, stubs keep the app compiling and
//  discovery still lists peer apps (install reports the feature is disabled).
// ============================================================

#[derive(Debug, Serialize)]
pub(crate) struct P2pStatus {
    enabled: bool,
    #[serde(rename = "nodeId")]
    node_id: String,
}

/// Module-level accessor for our stable iroh EndpointId (used by the chat layer
/// to publish it on-chain and to bootstrap gossip topics).
pub(crate) async fn node_id(app: &tauri::AppHandle) -> Result<String, String> {
    p2p_impl::node_id(app).await
}

/// v1.135.190: Endpoint узла для сторонних протоколов (webproxy → pq-web).
/// None — узел ещё не поднят (ensure) или фича p2p выключена.
#[cfg(feature = "p2p")]
pub(crate) fn endpoint_now() -> Option<iroh::Endpoint> {
    p2p_impl::endpoint_now()
}

/// Отдельный endpoint Mycelium (ключ `mycelium.key`, те же сетевые настройки, что у узла).
#[cfg(feature = "p2p")]
pub(crate) async fn mycelium_endpoint(app: &tauri::AppHandle) -> Result<iroh::Endpoint, String> {
    p2p_impl::isolated_endpoint(app, "mycelium.key").await
}

/// Медиа-endpoint мессенджера, если уже поднят (для диагностики соединений).
#[cfg(feature = "p2p")]
pub(crate) fn media_endpoint_now() -> Option<iroh::Endpoint> {
    p2p_impl::media_endpoint_now()
}

/// Гарантированно поднять узел и вернуть Endpoint.
#[cfg(feature = "p2p")]
pub(crate) async fn endpoint_ensure(app: &tauri::AppHandle) -> Result<iroh::Endpoint, String> {
    p2p_impl::endpoint_ensure(app).await
}

/// Endpoint чата (gossip + chat-dm + chat-sync) для `chat::sync` — None до старта узла.
#[cfg(feature = "p2p")]
pub(crate) async fn chat_endpoint() -> Option<iroh::Endpoint> {
    p2p_impl::endpoint_now()
}

/// Наш EndpointId чата (hex) — исключить себя из списка участников.
#[cfg(feature = "p2p")]
pub(crate) fn chat_endpoint_id() -> Option<String> {
    my_endpoint_id_hex()
}

/// Уведомить iroh-эндпоинт о смене сети (no-op без фичи p2p / до старта узла).
pub(crate) async fn notify_network_change() {
    p2p_impl::notify_network_change().await
}


#[cfg(not(feature = "p2p"))]
mod p2p_impl {
    use tauri::AppHandle;

    pub fn enabled() -> bool {
        false
    }
    pub async fn node_id(_app: &AppHandle) -> Result<String, String> {
        Ok(String::new())
    }
    pub async fn seed_all(_app: &AppHandle) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
    pub async fn seed_file(_app: &AppHandle, _path: &str) -> Result<String, String> {
        Ok(String::new())
    }
    pub async fn fetch_file(
        _app: &AppHandle,
        _node_id: &str,
        _hash: &str,
        _dest: &str,
        _extra_providers: &[String],
    ) -> Result<String, String> {
        Err("p2p disabled".to_string())
    }
    pub async fn chat_direct_send(
        _app: &AppHandle,
        _node: &str,
        _room_id: &str,
        _cipher: &str,
    ) -> bool {
        false
    }
    pub async fn media_node_id(_app: &AppHandle) -> Result<String, String> {
        Ok(String::new())
    }
    pub async fn fetch_media(
        _app: &AppHandle,
        _node_id: &str,
        _hash: &str,
        _dest: &str,
        _extra_providers: &[String],
    ) -> Result<String, String> {
        Err("p2p disabled".to_string())
    }
    pub async fn fetch_logo(
        _app: &AppHandle,
        _hash: &str,
        _node_ids: &[String],
        _dest: &std::path::Path,
    ) -> Result<(), String> {
        Err("p2p disabled".to_string())
    }
    pub async fn blob_progress(_app: &AppHandle, _hash: &str) -> (u64, bool) {
        (0, false)
    }
    pub fn announce_dev(_address: &str) {}
    pub fn register_dev_index_hash(_address: &str, _hash_hex: &str) {}
    pub fn dev_index_hash(_address: &str) -> Option<String> {
        None
    }
    pub fn announce_extra(_ih: [u8; 20], _hash_hex: &str) {}
    pub async fn discover_extra(
        _ih: [u8; 20],
        _cache_key: &str,
    ) -> (Vec<String>, Option<String>, u32) {
        (Vec::new(), None, 0)
    }
    pub async fn dev_discover(_address: &str) -> (Vec<String>, Option<String>, u32) {
        (Vec::new(), None, 0)
    }
    pub fn blob_store_bytes(_app: &AppHandle) -> u64 {
        0
    }
    pub async fn install(
        _app: &AppHandle,
        _id: &str,
        _node_id: &str,
        _hash: &str,
    ) -> Result<(), String> {
        Err("p2p feature disabled — rebuild with: cargo build --features p2p".to_string())
    }
    pub async fn prefetch(
        _app: &AppHandle,
        _node_id: &str,
        _hash: &str,
    ) -> Result<(), String> {
        Err("p2p feature disabled — rebuild with: cargo build --features p2p".to_string())
    }

    pub async fn probe_peer_storage(_app: &AppHandle, _node_id: &str, _hash: &str) -> String {
        "unverified".to_string()
    }

    /// Без фичи p2p эндпоинта нет — уведомлять некого.
    pub async fn notify_network_change() {}

    /// The blob store is never loaded without the `p2p` feature.
    pub fn is_active() -> bool {
        false
    }

    pub async fn peer_transports(_ids: Vec<String>) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
}

/// Mycelium (Фаза 2, скрытый приём): конверт chat-dm, пришедший через rendezvous, обрабатывается
/// тем же кодом, что и прямое соединение; ответ — `ok`.
#[cfg(feature = "p2p")]
pub(crate) fn mycelium_dm_accept(buf: &[u8]) -> Option<Vec<u8>> {
    p2p_impl::dm_accept_envelope(buf).then(|| b"ok".to_vec())
}

#[cfg(feature = "p2p")]
mod p2p_impl {
    use super::{ensure_data_dir, Lazy};
    use iroh::endpoint::presets;
    use iroh::{protocol::Router, Endpoint, EndpointId};
    use iroh_blobs::store::fs::options::Options;
    use iroh_blobs::store::{GcConfig, ProtectCb, ProtectOutcome};
    use iroh_blobs::api::proto::BlobStatus;
    use iroh_blobs::{store::fs::FsStore, BlobsProtocol, Hash};
    use iroh_mdns_address_lookup::MdnsAddressLookup;
    use iroh_mainline_address_lookup::DhtAddressLookup;
    use n0_mainline::Dht;
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use std::sync::Arc;
    use tauri::AppHandle;

    // Live keep-set for blob GC: the hashes this client currently seeds (its
    // published / installed dApps). `None` = not computed yet → GC is skipped so
    // we never wipe blobs before we know what to keep.
    pub(super) static SEED_HASHES: Lazy<std::sync::Mutex<Option<HashSet<Hash>>>> =
        Lazy::new(|| std::sync::Mutex::new(None));

    /// Live download byte counter keyed by blob hash string. Updated in real time
    /// by a concurrent `observe` task during a fetch so the UI progress bar moves
    /// mid-transfer (blob store `status()` only reports Partial size, which lags).
    static DL_BYTES: Lazy<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

    // Held alive for the process lifetime via std::mem::forget so the iroh
    // Router keeps serving; fields aren't read back (handles are cached in P2P).
    #[allow(dead_code)]
    struct P2p {
        endpoint: Endpoint,
        store: FsStore,
        node_id: String,
        _router: Router,
    }

    static P2P: Lazy<std::sync::Mutex<Option<(String, FsStore, Endpoint)>>> =
        Lazy::new(|| std::sync::Mutex::new(None));

    // =====================================================================
    //  ConnectionPool (v1.55.28) — переиспользование QUIC-соединений к
    //  провайдерам api://. Раньше каждый dApp fetch делал `endpoint.connect`
    //  заново — 300-800ms hole-punch на КАЖДЫЙ запрос. Теперь:
    //   • первый запрос устанавливает connection, кладёт в pool,
    //   • все следующие переиспользуют existing connection и просто
    //     открывают новый bi-stream (единицы мс),
    //   • provider с v0.5+ обрабатывает multiple accept_bi loop'ом.
    //  Совместимость: старые provider'ы (<0.5) закрывают connection после
    //  первого stream → open_bi() второй раз вернёт ошибку, мы удаляем
    //  такой connection из pool и восстанавливаем через новый connect.
    //  =====================================================================
    struct PooledConn {
        conn: iroh::endpoint::Connection,
        last_used: std::time::Instant,
        /// Какой ALPN был использован при connect. Определяет формат
        /// handshake на всех новых bi-stream'ах этого соединения.
        v1: bool,
    }
    static CONN_POOL: Lazy<
        tokio::sync::Mutex<std::collections::HashMap<EndpointId, PooledConn>>,
    > = Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));
    static CONN_POOL_GC_STARTED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    /// Single-flight: параллельные запросы к одному узлу без соединения в пуле ждут
    /// ОДИН connect, а не открывают по QUIC-соединению каждый (на старте кошелёк,
    /// стрим, пинги и мессенджер давали шторм из 10–20 connect'ов к одному узлу →
    /// «no available capacity» на реле, таймауты и ложный «P2P-узлы недоступны»).
    static CONNECTING: Lazy<tokio::sync::Mutex<std::collections::HashMap<EndpointId, std::sync::Arc<tokio::sync::Mutex<()>>>>> =
        Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

    async fn pool_hit(node_id: &EndpointId) -> Option<(iroh::endpoint::Connection, bool)> {
        let mut g = CONN_POOL.lock().await;
        let dead = g.get(node_id)?.conn.close_reason().is_some();
        if dead {
            g.remove(node_id);
            return None;
        }
        let pc = g.get_mut(node_id)?;
        pc.last_used = std::time::Instant::now();
        Some((pc.conn.clone(), pc.v1))
    }

    /// Максимум QUIC-соединений в pool. LRU-eviction по `last_used` при
    /// превышении. 32 хватит любому dashboard'у dApp'ов с большим запасом.
    const POOL_MAX_CONNS: usize = 32;
    /// Проактивная эвикция: если connection не использовался N секунд —
    /// закрываем сами, не ждём iroh keep-alive/idle-timeout.
    const POOL_IDLE_EVICT_SEC: u64 = 300;
    /// Период GC-таска.
    const POOL_GC_INTERVAL_SEC: u64 = 30;

    /// ALPN клиента: v1 предпочтительный (multi-request + length-prefix
    /// handshake + WS-tunnels), v0 — legacy fallback.
    pub(super) const NETFORY_ALPN_V0: &[u8] = b"netfory/api/0";
    pub(super) const NETFORY_ALPN_V1: &[u8] = b"netfory/api/1";

    /// Прямая доставка чат-конвертов (минуя gossip): ALPN chat-dm.
    /// v1 — одноразовое соединение (legacy-клиенты), v2 — мультиплекс: одно
    /// тёплое QUIC-соединение переиспользуется для ВСЕХ сообщений — мгновенная
    /// 1-на-1 доставка без нового рукопожатия на каждый конверт.
    pub(super) const CHAT_DM_ALPN: &[u8] = b"netfory/chat-dm/1";
    pub(super) const CHAT_DM_ALPN_V2: &[u8] = b"netfory/chat-dm/2";

    // Пул тёплых chat-dm/2 соединений: node -> живой Connection.
    static DM_POOL: Lazy<
        tokio::sync::Mutex<std::collections::HashMap<EndpointId, iroh::endpoint::Connection>>,
    > = Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

    /// Общий разбор входящего конверта прямой доставки (v1 и v2): JSON
    /// {room, cipher} → тот же конвейер, что и у gossip (расшифровка, дедуп,
    /// квитанция).
    pub(super) fn dm_accept_envelope(buf: &[u8]) -> bool {
        if let Ok(j) = serde_json::from_slice::<serde_json::Value>(buf) {
            let room = j["room"].as_str().unwrap_or("").to_string();
            let cipher = j["cipher"].as_str().unwrap_or("").to_string();
            if !room.is_empty() && room.len() <= 128 && !cipher.is_empty() {
                crate::chat::gossip::handle_direct(room, cipher);
                return true;
            }
        }
        false
    }

    /// v1-приёмник (legacy): одно соединение = один конверт.
    #[derive(Debug, Clone)]
    pub(super) struct ChatDirect;

    impl iroh::protocol::ProtocolHandler for ChatDirect {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            crate::peerbook::remember_id(connection.remote_id(), crate::peerbook::Role::Chat);
            let (mut send, mut recv) = connection.accept_bi().await?;
            let buf = recv
                .read_to_end(64 * 1024)
                .await
                .map_err(iroh::protocol::AcceptError::from_err)?;
            if dm_accept_envelope(&buf) {
                let _ = send.write_all(b"ok").await;
            }
            let _ = send.finish();
            connection.closed().await;
            Ok(())
        }
    }

    /// v2-приёмник: ОДНО тёплое соединение обслуживает много конвертов —
    /// каждый конверт = отдельный bi-stream. Основной мгновенный канал 1-на-1.
    #[derive(Debug, Clone)]
    pub(super) struct ChatDirectV2;

    impl iroh::protocol::ProtocolHandler for ChatDirectV2 {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            // входящий собеседник — в sled-книгу пиров (обратный dial без discovery)
            crate::peerbook::remember_id(connection.remote_id(), crate::peerbook::Role::Chat);
            loop {
                let (mut send, mut recv) = match connection.accept_bi().await {
                    Ok(s) => s,
                    Err(_) => break, // пир закрыл тёплый канал — норма
                };
                let Ok(buf) = recv.read_to_end(64 * 1024).await else { break };
                if dm_accept_envelope(&buf) {
                    let _ = send.write_all(b"ok").await;
                }
                let _ = send.finish();
            }
            Ok(())
        }
    }

    /// Один конверт по свежему bi-stream'у соединения; ждём "ok".
    async fn dm_push(conn: &iroh::endpoint::Connection, room_id: &str, cipher: &str) -> bool {
        let Ok((mut send, mut recv)) = conn.open_bi().await else {
            return false;
        };
        let payload = serde_json::json!({ "room": room_id, "cipher": cipher }).to_string();
        if send.write_all(payload.as_bytes()).await.is_err() || send.finish().is_err() {
            return false;
        }
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(8), recv.read_to_end(16)).await,
            Ok(Ok(b)) if b == b"ok"
        )
    }

    /// Прямая доставка чат-конверта. Порядок: тёплое v2-соединение из пула →
    /// свежий dial v2 (успех кладётся в пул) → legacy v1 (старые клиенты).
    /// true = получатель принял (дедуп/квитанция на его стороне).
    pub async fn chat_direct_send(
        app: &AppHandle,
        node: &str,
        room_id: &str,
        cipher: &str,
    ) -> bool {
        let Ok((_, _, endpoint)) = ensure(app).await else {
            return false;
        };
        let Ok(id) = node.parse::<EndpointId>() else {
            return false;
        };
        // 0) Mycelium вкл: конверт через цепочку Guard → Middle → Exit (fail-closed — без прямого
        //    пути; конверт остаётся в очереди повторов чата)
        let payload = serde_json::json!({ "room": room_id, "cipher": cipher }).to_string();
        if let Some(r) = crate::mycel::try_request(id, CHAT_DM_ALPN_V2, payload.as_bytes(), 16).await {
            return matches!(r, Ok(b) if b == b"ok");
        }
        // 1) тёплый канал из пула — мгновенно, без рукопожатия
        let pooled = DM_POOL.lock().await.get(&id).cloned();
        if let Some(conn) = pooled {
            if conn.close_reason().is_none() && dm_push(&conn, room_id, cipher).await {
                return true;
            }
            DM_POOL.lock().await.remove(&id);
        }
        // 2) свежий v2-dial → в пул на будущее
        if let Ok(Ok(conn)) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            endpoint.connect(id, CHAT_DM_ALPN_V2),
        )
        .await
        {
            crate::peerbook::remember(&endpoint, id, crate::peerbook::Role::Chat).await;
            if dm_push(&conn, room_id, cipher).await {
                DM_POOL.lock().await.insert(id, conn);
                return true;
            }
            conn.close(0u32.into(), b"dead");
        }
        // 3) legacy v1 — одноразовое соединение (клиенты ≤1.135.60)
        let conn = match tokio::time::timeout(
            std::time::Duration::from_secs(12),
            endpoint.connect(id, CHAT_DM_ALPN),
        )
        .await
        {
            Ok(Ok(c)) => c,
            _ => return false,
        };
        crate::peerbook::remember(&endpoint, id, crate::peerbook::Role::Chat).await;
        let ok = dm_push(&conn, room_id, cipher).await;
        conn.close(0u32.into(), b"done");
        ok
    }

    /// connect по NodeID с адресами из sled-книги пиров; при неудаче кэша —
    /// «голый» NodeID (discovery) и сброс устаревшей записи. Общий дедлайн `deadline`.
    pub(super) async fn connect_known(
        endpoint: &Endpoint,
        node_id: EndpointId,
        alpn: &'static [u8],
        deadline: std::time::Duration,
    ) -> Result<iroh::endpoint::Connection, String> {
        let r = connect_known_inner(endpoint, node_id, alpn, deadline).await;
        if let Err(e) = &r {
            crate::netstats::note_connect_error(e);
        }
        r
    }

    async fn connect_known_inner(
        endpoint: &Endpoint,
        node_id: EndpointId,
        alpn: &'static [u8],
        deadline: std::time::Duration,
    ) -> Result<iroh::endpoint::Connection, String> {
        let started = std::time::Instant::now();
        if let Some(addr) = crate::peerbook::cached(&node_id) {
            // кэшу даём не больше половины бюджета — остальное на discovery
            match tokio::time::timeout(deadline / 2, endpoint.connect(addr, alpn)).await {
                Ok(Ok(c)) => return Ok(c),
                Ok(Err(e)) => {
                    // ALPN-mismatch — не адресная проблема, книгу не трогаем
                    let msg = e.to_string();
                    if !msg.to_ascii_lowercase().contains("alpn") {
                        crate::peerbook::forget(&node_id);
                    }
                }
                Err(_) => crate::peerbook::forget(&node_id),
            }
        }
        let left = deadline.saturating_sub(started.elapsed()).max(std::time::Duration::from_secs(3));
        let err = match tokio::time::timeout(left, endpoint.connect(node_id, alpn)).await {
            Ok(Ok(c)) => return Ok(c),
            Ok(Err(e)) => e.to_string(),
            Err(_) => "таймаут подключения".to_string(),
        };
        // ALPN-mismatch — узел найден, просто другой протокол: подсказки не помогут
        if err.to_ascii_lowercase().contains("alpn") {
            return Err(err);
        }
        // v1.135.215: discovery не нашёл узел пула sth-core → адресные подсказки с трекера
        // (relay/IP, присланные другими клиентами). Трекер недоступен — просто ошибка выше.
        if let Some(addr) = crate::sth_hints::addr_for(&node_id).await {
            let short = node_id.to_string();
            match tokio::time::timeout(std::time::Duration::from_secs(8), endpoint.connect(addr, alpn)).await {
                Ok(Ok(c)) => {
                    crate::syslog::log("net", format!("узел {}… подключён по подсказке трекера (discovery не нашёл)", &short[..10]));
                    crate::peerbook::remember(endpoint, node_id, crate::peerbook::Role::Wallet).await;
                    return Ok(c);
                }
                Ok(Err(e)) => return Err(format!("{err}; по подсказке трекера: {e}")),
                Err(_) => return Err(format!("{err}; по подсказке трекера: таймаут")),
            }
        }
        Err(err)
    }

    /// Взять живой connection к nodeId из pool или установить новый.
    /// Пытается сначала V1 (WS + multi-req); при AlpnMismatch фолбэк на V0.
    /// Возвращает `(Connection, is_v1)` — вызывающий использует v1 для решения
    /// формата handshake (length-prefixed vs raw JSON+EOF) и для disallow'а
    /// WS на V0 connection'ах.
    pub(super) async fn pool_get_or_connect(
        endpoint: &Endpoint,
        node_id: EndpointId,
    ) -> Result<(iroh::endpoint::Connection, bool), String> {
        // 1) Fast path — pool hit + жив.
        if let Some(c) = pool_hit(&node_id).await {
            return Ok(c);
        }
        // 1b) Single-flight: один connect на узел; остальные ждут и берут из пула.
        let lock = CONNECTING.lock().await.entry(node_id).or_default().clone();
        let _flight = lock.lock().await;
        if let Some(c) = pool_hit(&node_id).await {
            return Ok(c);
        }

        // 2) Пробуем V1 сначала (12s deadline на весь процесс включая fallback).
        //    Адреса пира — из sled-книги (relay/IP с прошлого соединения), чтобы не
        //    ждать discovery; если кэш мёртв — повтор «голым» NodeID через discovery.
        let deadline = std::time::Duration::from_secs(12);
        let (conn, v1) = match connect_known(endpoint, node_id, NETFORY_ALPN_V1, deadline).await {
            Ok(c) => (c, true),
            Err(_e_v1) => {
                // Провайдер не поддерживает /1 → фолбэк на /0. Оставляем
                // достаточно времени на второй connect (обычно провайдер
                // возвращает ошибку ALPN очень быстро, ≤ 500ms).
                match connect_known(endpoint, node_id, NETFORY_ALPN_V0, std::time::Duration::from_secs(10)).await {
                    Ok(c) => (c, false),
                    Err(e) => return Err(format!("не удалось подключиться к провайдеру: {e}")),
                }
            }
        };
        crate::peerbook::remember(endpoint, node_id, crate::peerbook::Role::Api).await;

        // 3) LRU-cap 32.
        let mut g = CONN_POOL.lock().await;
        if g.len() >= POOL_MAX_CONNS {
            if let Some(oldest_key) = g
                .iter()
                .min_by_key(|(_, pc)| pc.last_used)
                .map(|(k, _)| *k)
            {
                if let Some(pc) = g.remove(&oldest_key) {
                    pc.conn.close(0u32.into(), b"pool-lru-evict");
                }
            }
        }
        g.insert(
            node_id,
            PooledConn {
                conn: conn.clone(),
                last_used: std::time::Instant::now(),
                v1,
            },
        );

        if !CONN_POOL_GC_STARTED.swap(true, std::sync::atomic::Ordering::AcqRel) {
            tokio::spawn(pool_gc_loop());
        }
        Ok((conn, v1))
    }

    /// Явное удаление connection из pool — вызывается, когда потребитель
    /// поймал ошибку `open_bi` / `read_to_end` (провайдер закрыл conn).
    pub(super) async fn pool_drop(node_id: &EndpointId) {
        let mut g = CONN_POOL.lock().await;
        if let Some(pc) = g.remove(node_id) {
            pc.conn.close(0u32.into(), b"caller-drop");
        }
    }

    /// Фоновый GC: раз в 30s чистим мёртвых и проактивно закрываем idle >5min.
    /// Работает вечно; всегда живёт ровно один инстанс через
    /// `CONN_POOL_GC_STARTED`.
    async fn pool_gc_loop() {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(POOL_GC_INTERVAL_SEC));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let mut g = CONN_POOL.lock().await;
            let now = std::time::Instant::now();
            let mut to_drop: Vec<EndpointId> = Vec::new();
            for (nid, pc) in g.iter() {
                if pc.conn.close_reason().is_some() {
                    to_drop.push(*nid);
                    continue;
                }
                if now.saturating_duration_since(pc.last_used).as_secs() > POOL_IDLE_EVICT_SEC {
                    to_drop.push(*nid);
                }
            }
            for nid in &to_drop {
                if let Some(pc) = g.remove(nid) {
                    pc.conn.close(0u32.into(), b"pool-idle-gc");
                }
            }
            if !to_drop.is_empty() {
                crate::syslog::log(
                    "p2p",
                    format!("pool-gc: evicted {} conn(s), pool_size={}", to_drop.len(), g.len()),
                );
            }
        }
    }


    pub fn enabled() -> bool {
        true
    }

    /// True only once the iroh blob store + endpoint are actually loaded for
    /// this session (i.e. the background GC is running). Lets callers avoid
    /// spinning up the node just to reclaim disk.
    pub fn is_active() -> bool {
        P2P.lock().unwrap().is_some()
    }

    /// Сеть вернулась (netwatch offline→online): подсказать iroh перепроверить
    /// интерфейсы/relay — на Windows после сна/смены Wi-Fi он сам это ловит не всегда,
    /// и QUIC-пути к соседям gossip остаются мёртвыми.
    pub async fn notify_network_change() {
        let ep = P2P.lock().unwrap().as_ref().map(|h| h.2.clone());
        if let Some(ep) = ep {
            ep.network_change().await;
        }
    }

    /// АСИНХРОННЫЙ резолв bootstrap-узлов Mainline DHT в IP:port.
    /// Зачем: n0_mainline в build() резолвит хосты СИНХРОННО (блокирует поток на
    /// таймаут DNS). Резолвим заранее без блокировки (concurrent, таймаут 1.5с) и
    /// отдаём IP-литералы — тогда build() не делает DNS и ничего не морозит.
    async fn resolve_dht_bootstrap() -> Vec<String> {
        // SmartNet-собственные публичные DHT bootstrap-сидеры (headless seeder с
        // открытым UDP). Гарантируют вход в DHT даже при недоступности публичных
        // BitTorrent-роутеров и без зависимости от n0/n1. Настраиваются через
        // Network Settings (cfg:seeders) или env SMARTNET_SEEDERS — поддерживается
        // НЕСКОЛЬКО сидеров (сеть не привязана к одному хосту).
        const PUBLIC_ROUTERS: [&str; 4] = [
            "router.bittorrent.com:6881",
            "dht.transmissionbt.com:6881",
            "dht.libtorrent.org:25401",
            "relay.pkarr.org:6881",
        ];
        let mut hosts: Vec<String> = super::get_seeders_internal();
        hosts.extend(PUBLIC_ROUTERS.iter().map(|s| s.to_string()));
        hosts.extend(super::load_dht_bootstrap());
        if let Ok(extra) = std::env::var("SMARTNET_DHT_BOOTSTRAP") {
            hosts.extend(
                extra
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        hosts.sort();
        hosts.dedup();
        let mut handles = Vec::new();
        for h in hosts {
            handles.push(tokio::spawn(async move {
                if h.parse::<std::net::SocketAddr>().is_ok() {
                    return vec![h];
                }
                match tokio::time::timeout(
                    std::time::Duration::from_millis(1500),
                    tokio::net::lookup_host(&h),
                )
                .await
                {
                    Ok(Ok(addrs)) => addrs
                        .filter(|a| a.is_ipv4())
                        .map(|a| a.to_string())
                        .collect(),
                    _ => Vec::new(),
                }
            }));
        }
        let mut out = Vec::new();
        for handle in handles {
            if let Ok(v) = handle.await {
                out.extend(v);
            }
        }
        out.sort();
        out.dedup();
        out
    }


    /// Транспорт живого соединения с каждым пиром: endpointId → "direct"|"relay".
    /// Пиры без активного соединения/инфо опускаются (фронт пометит как wan/mdns).
    pub async fn peer_transports(ids: Vec<String>) -> std::collections::HashMap<String, String> {
        let mut out = std::collections::HashMap::new();
        let ep = match P2P.lock().unwrap().as_ref() {
            Some(h) => h.2.clone(),
            None => return out,
        };
        for id in ids {
            let eid = match id.parse::<iroh::EndpointId>() {
                Ok(e) => e,
                Err(_) => continue,
            };
            if let Some(info) = ep.remote_info(eid).await {
                let mut kind: Option<&str> = None;
                for a in info.addrs() {
                    let k = if a.addr().is_relay() {
                        "relay"
                    } else if a.addr().is_ip() {
                        "direct"
                    } else {
                        continue;
                    };
                    let active = matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active);
                    if active {
                        kind = Some(k);
                        break;
                    }
                    if kind.is_none() {
                        kind = Some(k);
                    }
                }
                if let Some(k) = kind {
                    out.insert(id, k.to_string());
                }
            }
        }
        out
    }


    /// Добавляет SmartNet-реле в relay map свежесбинженного endpoint'а поверх
    /// дефолтных n0-реле: встроенные relay-*.sth.cx — ВСЕГДА (транспорт
    /// «последней надежды» по TCP/TLS 443 при мёртвом UDP), пользовательские
    /// n1-реле — при включённом тумблере. В стелс-режиме НЕ вызывается
    /// (см. call-site) — там строгая изоляция на кастомную relay map.
    async fn apply_smartnet_relays(endpoint: &Endpoint) {
        use iroh_relay::{RelayConfig, RelayQuicConfig};
        for u in super::effective_relay_urls() {
            match u.parse::<iroh::RelayUrl>() {
                Ok(url) => {
                    let rc = RelayConfig::new(url.clone(), Some(RelayQuicConfig::default()));
                    endpoint.insert_relay(url, Arc::new(rc)).await;
                }
                Err(e) => eprintln!("[n1-relay] skipping invalid relay url {u}: {e}"),
            }
        }
    }

    /// Результат dual-stack пробы одного реле по TCP/TLS 443.
    struct RelayTlsProbe {
        host: String,
        v4_ms: Option<u128>,
        v6_ms: Option<u128>,
    }

    impl RelayTlsProbe {
        fn ok(&self) -> bool {
            self.v4_ms.is_some() || self.v6_ms.is_some()
        }
        fn best(&self) -> Option<u128> {
            [self.v4_ms, self.v6_ms].into_iter().flatten().min()
        }
        fn summary(&self) -> String {
            let f = |m: &Option<u128>, tag: &str| match m {
                Some(ms) => format!("{tag} {ms}мс"),
                None => format!("{tag}✗"),
            };
            format!("{}: {} {}", self.host, f(&self.v4_ms, "v4"), f(&self.v6_ms, "v6"))
        }
    }

    /// DoH-резолв A/AAAA через Cloudflare по IP-литералу (https://1.1.1.1) —
    /// работает даже при полностью мёртвом/подменённом системном DNS.
    /// Блокирующий (ureq) — вызывать строго из spawn_blocking.
    fn doh_lookup_blocking(host: &str, rtype: &str) -> Vec<std::net::IpAddr> {
        let url = format!("https://1.1.1.1/dns-query?name={host}&type={rtype}");
        let body = match ureq::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .get(&url)
            .set("accept", "application/dns-json")
            .call()
        {
            Ok(r) => r.into_string().unwrap_or_default(),
            Err(_) => return Vec::new(),
        };
        serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("Answer").cloned())
            .and_then(|a| a.as_array().cloned())
            .map(|arr| {
                arr.iter()
                    // CNAME-записи в ответе не парсятся в IpAddr и отфильтруются сами
                    .filter_map(|r| r.get("data").and_then(|d| d.as_str()).and_then(|s| s.parse().ok()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Проба ОДНОЙ IP-семьи: честный TCP-connect на адреса из списка + реальный
    /// TLS-хендшейк со SNI = доменное имя из URL (фиксированный резолвер ureq
    /// подменяет только адреса, hostname для SNI/сертификата остаётся доменом) +
    /// HTTP GET. Любой HTTP-статус (даже 4xx) = TLS прошёл, реле досягаемо.
    /// Никакого QUIC/UDP — строго TCP 443.
    async fn probe_tls_family(addrs: Vec<std::net::SocketAddr>, url: String) -> Option<u128> {
        if addrs.is_empty() {
            return None;
        }
        tokio::task::spawn_blocking(move || {
            let t0 = std::time::Instant::now();
            let agent = ureq::builder()
                .timeout_connect(std::time::Duration::from_secs(4))
                .timeout(std::time::Duration::from_secs(6))
                .resolver(move |_netloc: &str| Ok::<_, std::io::Error>(addrs.clone()))
                .build();
            match agent.get(&url).call() {
                Ok(_) | Err(ureq::Error::Status(_, _)) => Some(t0.elapsed().as_millis()),
                Err(_) => None,
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Dual-stack проба реле по TCP/TLS 443: резолвим и A, и AAAA (системный
    /// DNS; недостающую семью добираем через DoH по IP-литералу), затем v4 и v6
    /// пробуем ПАРАЛЛЕЛЬНО и раздельно — оператор может резать IPv4 TCP,
    /// оставляя живым прямой IPv6-путь к реле (в обход CGNAT).
    async fn probe_relay_tls443(url: String) -> RelayTlsProbe {
        use std::net::SocketAddr;
        let host = url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split(['/', ':'])
            .next()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_string();
        let mut v4: Vec<SocketAddr> = Vec::new();
        let mut v6: Vec<SocketAddr> = Vec::new();
        if let Ok(Ok(addrs)) = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::lookup_host((host.clone(), 443u16)),
        )
        .await
        {
            for a in addrs {
                if a.is_ipv4() {
                    v4.push(a)
                } else {
                    v6.push(a)
                }
            }
        }
        // Системный DNS часто отдаёт только A (или вообще молчит) — добираем
        // недостающие записи через DoH, который оператор фильтровать не может.
        if v4.is_empty() || v6.is_empty() {
            let h = host.clone();
            let (need4, need6) = (v4.is_empty(), v6.is_empty());
            if let Ok((a4, a6)) = tokio::task::spawn_blocking(move || {
                (
                    if need4 { doh_lookup_blocking(&h, "A") } else { Vec::new() },
                    if need6 { doh_lookup_blocking(&h, "AAAA") } else { Vec::new() },
                )
            })
            .await
            {
                v4.extend(a4.into_iter().filter(|ip| ip.is_ipv4()).map(|ip| SocketAddr::new(ip, 443)));
                v6.extend(a6.into_iter().filter(|ip| ip.is_ipv6()).map(|ip| SocketAddr::new(ip, 443)));
            }
        }
        let (v4_ms, v6_ms) =
            tokio::join!(probe_tls_family(v4, url.clone()), probe_tls_family(v6, url.clone()));
        RelayTlsProbe { host, v4_ms, v6_ms }
    }

    /// v1.63.17: точечная проверка ПОЛЬЗОВАТЕЛЬСКИХ n1-реле — прогоняем те же
    /// dual-stack TCP/TLS 443 пробы (см. `probe_relay_tls443`), но по списку,
    /// который вводит пользователь. Каждая проба даёт свою запись в отчёте
    /// (`myrelay:<host>`) и эмитится событием `net-doctor-step` — UI показывает
    /// результат живьём под полем ввода. QUIC/UDP не трогаем: цель — понять,
    /// доступно ли конкретное реле на текущем канале по чистому TCP/TLS.
    pub(super) async fn doctor_check_relays(
        app: &AppHandle,
        urls: Vec<String>,
    ) -> Vec<super::DoctorCheck> {
        use super::doctor_push as push;
        let mut out = Vec::new();
        let urls: Vec<String> = urls
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if urls.is_empty() {
            push(app, &mut out, "myrelay:empty", "warn",
                "список пуст — введите хотя бы один URL реле (напр. https://relay.example)".to_string());
            return out;
        }
        // Пробуем параллельно, порядок ответа сохраняем через join_all.
        let mut handles = Vec::with_capacity(urls.len());
        for u in &urls {
            let u = u.clone();
            handles.push(tokio::spawn(async move { probe_relay_tls443(u.clone()).await }));
        }
        for (h, url) in handles.into_iter().zip(urls.into_iter()) {
            let r = match h.await {
                Ok(r) => r,
                Err(_) => {
                    push(app, &mut out, &format!("myrelay:{url}"), "fail",
                        format!("{url}: внутренняя ошибка пробы"));
                    continue;
                }
            };
            let id = format!("myrelay:{}", r.host);
            let status = if r.ok() { "ok" } else { "fail" };
            let detail = if r.ok() {
                format!("{}: досягаемо по TCP/TLS 443 · {}", r.host, r.summary())
            } else {
                format!("{}: не отвечает по TCP/TLS 443 (v4 и v6 закрыты)", r.host)
            };
            push(app, &mut out, &id, status, detail);
        }
        out
    }


    /// Пробуем открыть TCP-соединение по IPv6 к нескольким публичным
    /// хостам (одно из четырёх должно ответить). Возвращает `true`, если
    /// у клиента есть рабочий v6 outbound-путь. Используется только для
    /// диагностики: логгирует одну строку в scope=net при старте endpoint'а
    /// и НЕ влияет на решения биндинга (iroh сам биндит v6 при доступности).
    ///
    /// Соблюдает R1: `tokio::time::timeout` 2s на весь пробинг,
    /// TCP-connect чисто async через `tokio::net::TcpStream::connect`,
    /// без блокировки рантайма.
    pub(super) async fn probe_ipv6_available() -> bool {
        use tokio::net::TcpStream;
        // Публичные v6-адреса стабильных сервисов (443/tcp).
        // Пробуем несколько параллельно — первый ответ выигрывает.
        // Используем LITERAL v6-адреса, чтобы обойти DNS (иначе тест
        // проверит скорее наличие DNS AAAA-резолва, а не сам v6-путь).
        const V6_TARGETS: &[&str] = &[
            "[2606:4700:4700::1111]:443", // Cloudflare DNS
            "[2001:4860:4860::8888]:443", // Google DNS
            "[2620:fe::fe]:443",           // Quad9
        ];
        let deadline = std::time::Duration::from_secs(2);
        let mut futs = Vec::new();
        for addr in V6_TARGETS {
            let a = addr.to_string();
            futs.push(tokio::spawn(async move {
                tokio::time::timeout(deadline, TcpStream::connect(&a)).await.is_ok()
            }));
        }
        // Если хоть один вернул `Ok(true)` — v6 доступен.
        for f in futs {
            if let Ok(true) = f.await {
                return true;
            }
        }
        false
    }

    /// «Сетевой доктор» (v1.63.13): последовательные пробы для диагностики
    /// мобильных сетей (CGNAT / DPI-блокировка UDP / подмена DNS). Каждый шаг
    /// эмитится событием `net-doctor-step` — UI показывает прогресс живьём;
    /// полный отчёт возвращается по завершении и дублируется в SystemConsole.
    pub(super) async fn doctor_run(app: &AppHandle) -> Vec<super::DoctorCheck> {
        use super::doctor_push as push;
        use std::time::Duration;
        let mut out = Vec::new();

        // 1) Исходящий IPv6 — прямой обход CGNAT, если есть у обеих сторон.
        let has_v6 = probe_ipv6_available().await;
        push(app, &mut out, "ipv6", if has_v6 { "ok" } else { "warn" },
            if has_v6 {
                "IPv6 outbound works — two v6 nodes connect directly, bypassing CGNAT".to_string()
            } else {
                "no outbound IPv6 — on mobile networks a direct connect will hit CGNAT".to_string()
            });

        // 2) P2P-узел + фактический dual-stack бинд iroh-сокетов.
        let ep = match ensure(app).await {
            Ok((_, _, ep)) => ep,
            Err(e) => {
                push(app, &mut out, "node", "fail", format!("P2P node failed to start: {e}"));
                push(app, &mut out, "verdict", "fail", "diagnostics aborted — node did not start".to_string());
                return out;
            }
        };
        let socks = ep.bound_sockets();
        let v6n = socks.iter().filter(|s| s.is_ipv6()).count();
        push(app, &mut out, "sockets", if v6n > 0 { "ok" } else { "warn" },
            format!(
                "iroh bound: {}{}",
                socks.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" · "),
                if v6n == 0 { " (IPv4 only — no IPv6 stack available on this host)" } else { "" }
            ));

        // 3) net_report iroh: QAD-пробы (QUIC/UDP до реле), NAT-тип, задержки реле.
        use iroh::Watcher as _;
        let mut udp_ok = false;
        let mut symmetric: Option<bool> = None;
        match tokio::time::timeout(Duration::from_secs(20), ep.net_report().initialized()).await {
            Ok(r) => {
                udp_ok = r.udp_v4 || r.udp_v6;
                symmetric = r.mapping_varies_by_dest();
                let mut urls: Vec<String> =
                    r.relay_latency.iter().map(|(_, u, _)| u.to_string()).collect();
                urls.sort();
                urls.dedup();
                let best = r.relay_latency.iter().map(|(_, _, d)| d.as_millis()).min();
                push(app, &mut out, "udp", if udp_ok { "ok" } else { "fail" },
                    format!(
                        "QUIC/UDP outbound: v4={} · v6={} (QAD probes to relays)",
                        if r.udp_v4 { "yes" } else { "no" },
                        if r.udp_v6 { "yes" } else { "no" }
                    ));
                push(app, &mut out, "nat",
                    match symmetric { Some(true) => "warn", Some(false) => "ok", None => "warn" },
                    match symmetric {
                        Some(true) => "Symmetric NAT (typical mobile CGNAT): hole-punch does not work; direct only when both sides have IPv6, otherwise relay".to_string(),
                        Some(false) => format!(
                            "Cone NAT: hole-punch possible{}",
                            r.global_v4.map(|a| format!(" · external address {a}")).unwrap_or_default()
                        ),
                        None => "NAT type unknown (too few UDP responses — UDP is likely filtered)".to_string(),
                    });
                push(app, &mut out, "relay", if !urls.is_empty() { "ok" } else { "warn" },
                    if !urls.is_empty() {
                        format!(
                            "{} relays reachable via QUIC, best latency {} ms{}",
                            urls.len(),
                            best.unwrap_or(0),
                            r.preferred_relay.as_ref().map(|u| format!(" · home: {u}")).unwrap_or_default()
                        )
                    } else {
                        "no relay responded via QUIC (UDP path to relays is blocked)".to_string()
                    });
            }
            _ => {
                push(app, &mut out, "udp", "fail",
                    "net_report did not finish within 20s — DPI likely drops all unknown UDP".to_string());
                push(app, &mut out, "nat", "warn", "NAT type unknown (no UDP responses)".to_string());
                push(app, &mut out, "relay", "warn", "QUIC probes to relays unavailable".to_string());
            }
        }

        // 4) Реле по HTTPS (TCP/TLS 443) — транспорт «последней надежды» при
        //    полностью зарезанном UDP. Честная проба: реальный TCP-connect +
        //    TLS-хендшейк со SNI по доменному имени + HTTP GET (НЕ QUIC/UDP).
        //    Dual-stack: A и AAAA резолвятся оба (при мёртвом системном DNS —
        //    через DoH к 1.1.1.1 по IP-литералу), v4 и v6 пробуются раздельно.
        //    В списке — встроенные SmartNet-реле (relay-*.sth.cx) + n0-реле.
        let mut relay_urls: Vec<String> = super::effective_relay_urls();
        for u in [
            "https://euc1-1.relay.n0.iroh.link".to_string(),
            "https://use1-1.relay.n0.iroh.link".to_string(),
        ] {
            if !relay_urls.contains(&u) {
                relay_urls.push(u);
            }
        }
        relay_urls.truncate(6);
        let total = relay_urls.len();
        let mut handles = Vec::new();
        for u in relay_urls {
            handles.push(tokio::spawn(probe_relay_tls443(u)));
        }
        let mut https_ok = 0usize;
        let mut https_best: Option<u128> = None;
        let (mut v4_any, mut v6_any) = (false, false);
        let mut parts: Vec<String> = Vec::new();
        for h in handles {
            if let Ok(r) = h.await {
                if r.ok() {
                    https_ok += 1;
                    if let Some(b) = r.best() {
                        https_best = Some(https_best.map_or(b, |x| x.min(b)));
                    }
                }
                v4_any |= r.v4_ms.is_some();
                v6_any |= r.v6_ms.is_some();
                parts.push(r.summary());
            }
        }
        let v6_note = if https_ok > 0 && !v4_any && v6_any {
            " · IPv4 TCP blocked by ISP — relays reachable over IPv6 TCP (bypassing CGNAT)"
        } else {
            ""
        };
        push(app, &mut out, "relay_https", if https_ok > 0 { "ok" } else { "fail" },
            format!(
                "{https_ok}/{total} relays reachable over TCP/TLS 443{} · {}{v6_note}",
                https_best.map(|b| format!(", best {b} ms")).unwrap_or_default(),
                parts.join(" · ")
            ));

        // 5) Mainline DHT: живая таблица + прямой BEP5-ping до bootstrap'ов
        //    (детект блокировки «нестандартного» UDP типа :6881).
        let dht_nodes = super::dht_stats().nodes;
        let boots = resolve_dht_bootstrap().await;
        let mut dht_ping = false;
        if !boots.is_empty() {
            let id = crate::dht_dapp::app_infohash(&format!(
                "doctor:{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let mut pkt = Vec::with_capacity(56);
            pkt.extend_from_slice(b"d1:ad2:id20:");
            pkt.extend_from_slice(&id);
            pkt.extend_from_slice(b"e1:q4:ping1:t2:aa1:y1:qe");
            if let Ok(sock) = tokio::net::UdpSocket::bind("0.0.0.0:0").await {
                for b in boots.iter().take(4) {
                    if let Ok(addr) = b.parse::<std::net::SocketAddr>() {
                        if addr.is_ipv4() {
                            let _ = sock.send_to(&pkt, addr).await;
                        }
                    }
                }
                let mut buf = [0u8; 512];
                dht_ping = tokio::time::timeout(Duration::from_secs(3), sock.recv_from(&mut buf))
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);
            }
        }
        push(app, &mut out, "dht", if dht_ping || dht_nodes > 0 { "ok" } else { "fail" },
            format!(
                "nodes in table: {dht_nodes} · BEP5-ping bootstrap: {}",
                if dht_ping { "response received" } else { "no response (UDP to DHT is likely blocked)" }
            ));

        // 6) DNS: системный резолв vs DoH (детект операторской подмены/блокировки).
        const DNS_HOST: &str = "router.bittorrent.com";
        let sys_ok = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::net::lookup_host((DNS_HOST, 6881_u16)),
        )
        .await
        .map(|r| r.map(|mut a| a.next().is_some()).unwrap_or(false))
        .unwrap_or(false);
        let doh_ok = tokio::task::spawn_blocking(|| {
            ureq::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .get(&format!("https://cloudflare-dns.com/dns-query?name={DNS_HOST}&type=A"))
                .set("accept", "application/dns-json")
                .call()
                .map(|r| r.into_string().unwrap_or_default().contains("\"Answer\""))
                .unwrap_or(false)
        })
        .await
        .unwrap_or(false);
        push(app, &mut out, "dns",
            if sys_ok { "ok" } else if doh_ok { "warn" } else { "fail" },
            match (sys_ok, doh_ok) {
                (true, _) => "system DNS works".to_string(),
                (false, true) => "system DNS not responding, but DoH (Cloudflare) works — ISP is filtering DNS".to_string(),
                (false, false) => "neither system DNS nor DoH respond".to_string(),
            });

        // 7) Verdict.
        let (vs, vd) = if udp_ok && has_v6 {
            ("ok", "direct P2P connections available: UDP open, IPv6 present — CGNAT is no obstacle")
        } else if udp_ok && symmetric == Some(true) {
            ("warn", "UDP open, but Symmetric NAT (CGNAT): direct only with v6 peers, everything else goes via relays. Network WORKS")
        } else if udp_ok {
            ("ok", "UDP open, NAT traversable — network works in direct mode")
        } else if udp_ok && symmetric == Some(true) {
            ("warn", "UDP open, but Symmetric NAT (CGNAT): direct only with v6 peers, everything else goes via relays. Network WORKS")
        } else if udp_ok {
            ("ok", "UDP open, NAT traversable — network works in direct mode")
        } else if https_ok > 0 {
            ("warn", "UDP blocked by ISP. Network runs via relays over TCP/TLS 443 — SmartNet relays (relay-*.sth.cx) are built in, traffic will automatically route through them")
        } else {
            ("fail", "neither UDP nor relays over TCP/TLS 443 are reachable — the network cannot operate in this environment")
        };
        push(app, &mut out, "verdict", vs, vd.to_string());
        out
    }

    /// DoH-резолвер (DNS-over-HTTPS, RFC 8484) для мобильных сетей, где оператор
    /// фильтрует/подменяет системный DNS. Nameserver'ы заданы IP-литералами
    /// (Cloudflare 1.1.1.1/1.0.0.1, Google 8.8.8.8) — резолвить сами DoH-серверы
    /// не нужно, а их TLS-сертификаты содержат IP-SAN, так что рукопожатие валидно.
    fn doh_resolver() -> iroh::dns::DnsResolver {
        use iroh::dns::DnsProtocol;
        use std::net::SocketAddr;
        // Системные резолверы остаются в списке (hickory сам отодвигает неотвечающие),
        // DoH — Cloudflare/Google/Quad9/AdGuard: один заблокированный провайдер не
        // должен ронять discovery EndpointId (TXT _iroh) целиком.
        iroh::dns::DnsResolver::builder()
            .with_system_defaults()
            .with_nameserver(SocketAddr::from(([1, 1, 1, 1], 443)), DnsProtocol::Https)
            .with_nameserver(SocketAddr::from(([8, 8, 8, 8], 443)), DnsProtocol::Https)
            .with_nameserver(SocketAddr::from(([9, 9, 9, 9], 443)), DnsProtocol::Https)
            .with_nameserver(SocketAddr::from(([94, 140, 14, 14], 443)), DnsProtocol::Https)
            .with_nameserver(SocketAddr::from(([1, 0, 0, 1], 443)), DnsProtocol::Https)
            .build()
    }

    /// Проба системного DNS для режима "auto": асинхронный резолв хоста n0-реле
    /// с таймаутом 2.5с. Не отвечает / пустой ответ → оператор, вероятно, ломает
    /// DNS → активируем фолбэки. Здоровые сети пробу проходят и НЕ затрагиваются.
    async fn system_dns_broken() -> bool {
        tokio::time::timeout(
            std::time::Duration::from_millis(2500),
            tokio::net::lookup_host(("euc1-1.relay.n0.iroh.link", 443)),
        )
        .await
        .map(|r| r.map(|mut a| a.next().is_none()).unwrap_or(true))
        .unwrap_or(true)
    }

    /// Активны ли мобильные фолбэки (Phase 2): "on" — всегда, "off" — никогда,
    /// "auto" — только когда системный DNS реально не отвечает.
    async fn mobile_fallbacks_active() -> bool {
        match super::mobile_fallbacks_mode().as_str() {
            "on" => true,
            "off" => false,
            _ => system_dns_broken().await,
        }
    }

    /// Стандартный endpoint (не-стелс): N0-пресет (n0-реле + DNS/pkarr discovery),
    /// плюс mDNS (если не relay-only) и Mainline DHT (если включён).
    async fn build_default_endpoint(
        secret: iroh::SecretKey,
        relay_only: bool,
        dht_on: bool,
        dht_boot_ips: &[String],
        mobile_fb: bool,
    ) -> Result<Endpoint, String> {
        let mut builder = Endpoint::builder(presets::N0).secret_key(secret);
        // ПРИОРИТЕТ n1-РЕЛЕ. Публичные n0-реле iroh — тестовая инфраструктура: на
        // суточном аптайме домашнее реле «тихо» умирало, gossip-топики теряли всех
        // соседей («ONLINE 0») и мессенджер переставал принимать сообщения. Если хотя
        // бы одно n1-реле отвечает по TLS/443 — RelayMap = ТОЛЬКО n1 (домашнее реле
        // выбирается из них); n0 остаются лишь запасным вариантом, когда все n1 недоступны.
        // DNS/pkarr-discovery пресета N0 при этом сохраняется (relay_mode меняет только реле).
        {
            use iroh::RelayMode;
            use iroh_relay::{RelayConfig, RelayMap, RelayQuicConfig};
            let urls = super::effective_relay_urls();
            let mut handles = Vec::new();
            for u in urls.iter().cloned() {
                handles.push(tokio::spawn(probe_relay_tls443(u)));
            }
            let mut alive: Vec<Arc<RelayConfig>> = Vec::new();
            for (h, u) in handles.into_iter().zip(urls.iter()) {
                if let Ok(Ok(p)) = tokio::time::timeout(std::time::Duration::from_secs(6), h).await {
                    if p.ok() {
                        if let Ok(url) = u.parse::<iroh::RelayUrl>() {
                            alive.push(Arc::new(RelayConfig::new(url, Some(RelayQuicConfig::default()))));
                        }
                    }
                }
            }
            if alive.is_empty() {
                crate::syslog::log("net", "n1-реле недоступны по TLS/443 — временно используем n0-реле iroh (fallback)");
            } else {
                crate::syslog::log("net", format!("RelayMap: {} n1-реле в приоритете, n0 отключены", alive.len()));
                builder = builder.relay_mode(RelayMode::Custom(RelayMap::from_iter(alive)));
            }
        }
        // Мобильные фолбэки: (1) DoH-резолвер — DNS-discovery (TXT _iroh) и резолв
        // hostname'ов реле идут по HTTPS/443 вместо фильтруемого UDP-DNS оператора;
        // (2) pkarr HTTPS-discovery — дополнительный резолвер EndpointId прямыми
        // HTTPS-запросами к pkarr-реле n0 (аддитивно к DNS-lookup из пресета N0,
        // резолверы опрашиваются конкурентно — здоровый путь не замедляется).
        // Транспорт данных к реле у iroh 1.0 и так WebSocket/TLS-443 (TCP), так
        // что при полностью зарезанном UDP связь автоматически пойдёт через реле.
        if mobile_fb {
            builder = builder
                .dns_resolver(doh_resolver())
                .address_lookup(iroh::address_lookup::PkarrResolver::n0_dns());
        }
        // mDNS: в обычном режиме — анонс + резолв LAN-пиров. В relay-only раньше mDNS
        // отключался целиком, и при «мобильных фолбэках» (DoH вместо системного DNS)
        // узлы sth-core из LAN/WAN переставали резолвиться вовсе (v1.135.209). Теперь
        // relay-only = только НЕ анонсируем свои LAN-адреса (advertise=false), но чужие
        // по-прежнему находим — приватность IP пользователя сохраняется.
        builder = builder.address_lookup(MdnsAddressLookup::builder().advertise(!relay_only));
        if dht_on {
            let mut db = Dht::builder();
            db.bootstrap(dht_boot_ips);
            builder = builder.address_lookup(DhtAddressLookup::builder().dht_builder(db));
        }
        // sled-книга известных пиров — мгновенные адреса без discovery (peerbook.rs)
        builder = builder.address_lookup(crate::peerbook::PeerbookLookup::open());
        builder.bind().await.map_err(|e| e.to_string())
    }

    /// СТЕЛС endpoint: строгая изоляция на кастомную relay-map (только наши
    /// n1-реле, RelayMode::Custom). presets::Minimal ставит лишь обязательный
    /// crypto provider — без дефолтных n0-реле и без n0-DNS/pkarr discovery.
    /// mDNS НЕ добавляем → узел не анонсирует прямые LAN-адреса; прямой WAN
    /// hole-punching не инициируется. Разрешение EndpointId идёт только через
    /// бессерверный Mainline DHT (pkarr). Данные к пирам инкапсулируются в
    /// QUIC-over-relay (TLS/443) к n1-реле.
    async fn build_stealth_endpoint(
        secret: iroh::SecretKey,
        dht_on: bool,
        dht_boot_ips: &[String],
        relay_urls: &[String],
        mobile_fb: bool,
    ) -> Result<Endpoint, String> {
        use iroh::RelayMode;
        use iroh_relay::{RelayConfig, RelayMap, RelayQuicConfig};
        let mut configs = Vec::new();
        for u in relay_urls {
            match u.parse::<iroh::RelayUrl>() {
                Ok(url) => {
                    configs.push(Arc::new(RelayConfig::new(url, Some(RelayQuicConfig::default()))))
                }
                Err(e) => crate::syslog::log(
                    "p2p",
                    format!("STEALTH: skipped invalid n1-relay {u}: {e}"),
                ),
            }
        }
        if configs.is_empty() {
            return Err("нет валидных n1-реле для стелс-изоляции".to_string());
        }
        let relay_map = RelayMap::from_iter(configs);
        // presets::Minimal ставит только обязательный crypto provider (без n0-реле
        // и без n0-DNS discovery). relay_mode(Custom) задаёт СТРОГО наши n1-реле.
        // mDNS не добавляем → нет анонса прямых LAN-адресов. Разрешение EndpointId —
        // только через бессерверный Mainline DHT (pkarr).
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(secret)
            .relay_mode(RelayMode::Custom(relay_map));
        // Мобильные фолбэки в стелсе: ТОЛЬКО DoH-резолвер (резолв hostname'ов
        // n1-реле по HTTPS вместо фильтруемого DNS). pkarr HTTPS-discovery через
        // n0-инфраструктуру НЕ добавляем — стелс сохраняет строгую relay-изоляцию,
        // приоритет всегда у стелс-релеев.
        if mobile_fb {
            builder = builder.dns_resolver(doh_resolver());
        }
        if dht_on {
            let mut db = Dht::builder();
            db.bootstrap(dht_boot_ips);
            builder = builder.address_lookup(DhtAddressLookup::builder().dht_builder(db));
        }
        // sled-книга пиров: в стелсе подсказывает ТОЛЬКО n1-реле оператора (relay-изоляция)
        builder = builder.address_lookup(crate::peerbook::PeerbookLookup::stealth(relay_urls));
        builder.bind().await.map_err(|e| e.to_string())
    }

    /// Живой iroh Endpoint для AI-клиента (ai://, ALPN netfory/ai/1).
    pub(super) async fn ai_endpoint(app: &AppHandle) -> Result<Endpoint, String> {
        ensure(app).await.map(|(_, _, e)| e)
    }

    pub(super) fn endpoint_now() -> Option<Endpoint> {
        P2P.lock().ok()?.as_ref().map(|h| h.2.clone())
    }

    /// v1.135.210: диагностика discovery — каким сервисом (dns / pkarr / mdns / mainline /
    /// sled-peerbook) и за сколько мс резолвится EndpointId, затем тестовый connect.
    /// IP-адреса в отчёт не попадают (только счётчики и хост реле).
    pub(super) async fn discovery_probe(id: EndpointId) -> super::DiscoveryReport {
        use n0_future::StreamExt;
        let mut rep = super::DiscoveryReport { id: id.to_string(), ..Default::default() };
        let Some(ep) = endpoint_now() else {
            rep.errors.push("P2P endpoint ещё не поднят".into());
            return rep;
        };
        if let Some(a) = crate::peerbook::cached(&id) {
            rep.hits.push(super::DiscoveryHit {
                source: "sled-peerbook".into(),
                ms: 0,
                relays: a.relay_urls().count(),
                addrs: a.ip_addrs().count(),
                relay: a.relay_urls().next().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default(),
            });
        }
        let t0 = std::time::Instant::now();
        match ep.address_lookup() {
            Ok(lookup) => {
                let mut stream = std::pin::pin!(lookup.resolve(id));
                let budget = std::time::Duration::from_secs(8);
                loop {
                    let left = budget.saturating_sub(t0.elapsed());
                    if left.is_zero() {
                        rep.errors.push("discovery: таймаут 8 с".into());
                        break;
                    }
                    match tokio::time::timeout(left, stream.next()).await {
                        Ok(Some(Ok(Ok(item)))) => {
                            let info = item.endpoint_info();
                            rep.hits.push(super::DiscoveryHit {
                                source: item.provenance().to_string(),
                                ms: t0.elapsed().as_millis() as u64,
                                relays: info.relay_urls().count(),
                                addrs: info.ip_addrs().count(),
                                relay: info.relay_urls().next().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default(),
                            });
                        }
                        Ok(Some(Ok(Err(e)))) => rep.errors.push(format!("{e}")),
                        Ok(Some(Err(e))) => {
                            rep.errors.push(format!("{e}"));
                            break;
                        }
                        Ok(None) => break,
                        Err(_) => {
                            rep.errors.push("discovery: таймаут 8 с".into());
                            break;
                        }
                    }
                }
            }
            Err(e) => rep.errors.push(format!("address_lookup: {e}")),
        }
        rep.lookup_ms = t0.elapsed().as_millis() as u64;
        // тестовый connect по ALPN sth-core: путь relay/direct и время
        let t1 = std::time::Instant::now();
        match tokio::time::timeout(std::time::Duration::from_secs(12), ep.connect(id, NETFORY_ALPN_V1)).await {
            Ok(Ok(conn)) => {
                rep.connect_ms = Some(t1.elapsed().as_millis() as u64);
                let paths = conn.paths();
                rep.via = if paths.iter().any(|p| p.is_selected() && p.is_relay()) { "relay".into() } else { "direct".into() };
                conn.close(0u32.into(), b"diag");
            }
            Ok(Err(e)) => rep.errors.push(format!("connect: {e}")),
            Err(_) => rep.errors.push("connect: таймаут 12 с".into()),
        }
        crate::syslog::log(
            "net",
            format!(
                "discovery {}: {} · lookup {} мс · connect {} ({})",
                &rep.id[..rep.id.len().min(10)],
                if rep.hits.is_empty() { "нет источников".to_string() } else { rep.hits.iter().map(|h| format!("{} {}мс", h.source, h.ms)).collect::<Vec<_>>().join(", ") },
                rep.lookup_ms,
                rep.connect_ms.map(|m| format!("{m} мс")).unwrap_or_else(|| "✗".into()),
                rep.via
            ),
        );
        rep
    }

    pub(super) async fn endpoint_ensure(app: &AppHandle) -> Result<Endpoint, String> {
        ensure(app).await.map(|h| h.2)
    }

    async fn ensure(app: &AppHandle) -> Result<(String, FsStore, Endpoint), String> {
        if let Some(h) = P2P.lock().unwrap().as_ref() {
            return Ok(h.clone());
        }
        let blobs_dir = ensure_data_dir(app).join("blobs");
        tokio::fs::create_dir_all(&blobs_dir).await.map_err(|e| e.to_string())?;
        // Blob GC: every 5 min the store keeps only the blobs whose hash is in
        // SEED_HASHES (our currently-seeded dApps) and deletes the rest (old
        // versions / abandoned downloads). Skipped until the first seed_all
        // computes the keep-set, so we never wipe blobs prematurely.
        let protect: ProtectCb = Arc::new(move |live: &mut HashSet<Hash>| {
            Box::pin(async move {
                match SEED_HASHES.lock().unwrap().as_ref() {
                    None => ProtectOutcome::Abort,
                    Some(set) => {
                        for h in set {
                            live.insert(*h);
                        }
                        ProtectOutcome::Continue
                    }
                }
            })
        });
        // blobs/temp — недописанные импорты/загрузки iroh-blobs. Стор чистит их
        // только при штатном завершении операции; после крэша/убитого процесса
        // они остаются навсегда (наблюдали 17 ГБ за 3 месяца). До открытия стора
        // ничего не может быть «в полёте» — сносим всё старше суток.
        purge_stale_temp(&blobs_dir.join("temp"));
        let mut options = Options::new(&blobs_dir);
        options.gc = Some(GcConfig {
            interval: std::time::Duration::from_secs(120),
            add_protected: Some(protect),
        });
        let store = FsStore::load_with_opts(blobs_dir.join("blobs.db"), options)
            .await
            .map_err(|e| e.to_string())?;
        // Снимаем постоянные auto-теги прошлых версий: без этого GC не мог удалить
        // ни одного старого блоба (теги — корни), и `blobs/` рос бесконечно.
        drop_legacy_tags(&store).await;
        // iroh 1.0: bind an endpoint with N0 discovery PLUS a LAN mDNS address
        // lookup so EndpointIds resolve to local addresses with NO internet —
        // this is what lets a second PC on the same network actually connect.
        //
        // Relay-only privacy mode (Settings toggle): when enabled, skip the LAN
        // mDNS address lookup so we neither advertise nor resolve direct local
        // addresses — connectivity is carried over the n0 relay, hiding the
        // user's direct IP from peers.
        let relay_only = super::relay_only_internal();
        // STABLE node identity: persist the iroh secret key under data/node.key so
        // the EndpointId stays the SAME across app restarts. Without this every
        // launch minted a fresh identity, so tracker/peer announces pointed at a
        // dead EndpointId (peers connect but can't fetch) and the Network Map
        // accumulated duplicates.
        let secret = {
            let key_path = ensure_data_dir(app).join("node.key");
            match tokio::fs::read(&key_path).await {
                Ok(b) if b.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&b);
                    iroh::SecretKey::from_bytes(&arr)
                }
                _ => {
                    let sk = iroh::SecretKey::generate();
                    let _ = tokio::fs::write(&key_path, sk.to_bytes()).await;
                    sk
                }
            }
        };
        // Mainline DHT (pkarr) discovery — бессерверное обнаружение NodeID через
        // сеть BitTorrent, поверх n0-DNS. Включается тумблером в Настройках →
        // Сеть (по умолчанию ВКЛ). Релеи остаются транспортным fallback за NAT.
        // Кэш живых DHT-узлов (cfg:dht_boot) подмешивается как extra_bootstrap —
        // узел поднимается в DHT за секунды даже без публичных bootstrap-серверов.
        let dht_on = super::dht_enabled_internal();
        // КРИТИЧНО: n0_mainline в build() резолвит bootstrap-хосты СИНХРОННО
        // (std to_socket_addrs) — при недоступном DNS это блокирует поток на
        // таймаут (10-12с) и морозит UI. Поэтому резолвим bootstrap ЗАРАНЕЕ,
        // АСИНХРОННО (с таймаутом) и передаём уже IP-литералы → build() не делает
        // блокирующий DNS. Если резолв не удался — пустой список (DHT просто не
        // забутстрапится, но НИЧЕГО не блокируется).
        let dht_boot_ips: Vec<String> = if dht_on {
            resolve_dht_bootstrap().await
        } else {
            Vec::new()
        };
        // Мобильные фолбэки (Phase 2): "auto" — проба системного DNS решает,
        // "on"/"off" — принудительно. При активации: DoH-резолвер + pkarr
        // HTTPS-discovery (в стелсе — только DoH).
        let mobile_fb = mobile_fallbacks_active().await;
        if mobile_fb {
            crate::syslog::log(
                "p2p",
                format!(
                    "mobile fallbacks ACTIVE (mode={}): DoH resolver + pkarr HTTPS discovery",
                    super::mobile_fallbacks_mode()
                ),
            );
        }
        // СТЕЛС-РЕЖИМ: полная relay-изоляция применяется при СТАРТЕ узла (после
        // перезапуска приложения — так безопаснее, без пересоздания живого
        // endpoint и без риска deadlock). Если включён и заданы n1-реле — строим
        // endpoint строго на кастомной relay-map. Иначе — стандартная сеть.
        let stealth = crate::stealth::stealth_enabled_persisted();
        let n1 = super::get_relay_config();
        let endpoint = if stealth && !n1.urls.is_empty() {
            // Байты ключа — на случай безопасного фолбэка (SecretKey переносится в
            // билдер по значению; реконструируем из байтов, без зависимости от Clone).
            let secret_bytes = secret.to_bytes();
            match build_stealth_endpoint(secret, dht_on, &dht_boot_ips, &n1.urls, mobile_fb).await {
                Ok(ep) => {
                    crate::syslog::log(
                        "p2p",
                        format!(
                            "STEALTH endpoint bound: {} (n1-relays={}, dht={}, direct-UDP=off)",
                            ep.id(),
                            n1.urls.len(),
                            dht_on
                        ),
                    );
                    ep
                }
                Err(e) => {
                    crate::syslog::log(
                        "p2p",
                        format!("STEALTH build failed ({e}) — safe fallback to standard network"),
                    );
                    let fb_secret = iroh::SecretKey::from_bytes(&secret_bytes);
                    build_default_endpoint(fb_secret, relay_only, dht_on, &dht_boot_ips, mobile_fb).await?
                }
            }
        } else {
            let ep = build_default_endpoint(secret, relay_only, dht_on, &dht_boot_ips, mobile_fb).await?;
            crate::syslog::log(
                "p2p",
                format!(
                    "endpoint bound: {} (relay_only={}, dht={})",
                    ep.id(),
                    relay_only,
                    dht_on
                ),
            );
            // IPv6-проба: узнаём, есть ли у клиента рабочий v6-путь наружу.
            // Логгируется отдельной строкой в scope=net — чтобы пользователь
            // мог понять, идёт ли трафик прямым v6 (без CGNAT-неопределённости
            // и без relay-накладок). Проба чисто клирнетовая (google favicon),
            // одна и на весь запуск — не спамим сеть.
            tokio::spawn(async move {
                let has_v6 = probe_ipv6_available().await;
                if has_v6 {
                    crate::syslog::log("net", "ipv6 outbound: OK (client can connect directly over v6)");
                } else {
                    crate::syslog::log("net", "ipv6 outbound: unavailable (IPv4 only; direct-connect may go via relay under CGNAT)");
                }
            });
            ep
        };
        // SmartNet n1-relay (не-стелс): добавляем реле оператора поверх n0.
        if !stealth {
            apply_smartnet_relays(&endpoint).await;
        }
        crate::peerbook::set_endpoint(&endpoint);
        // DHT-монитор: лёгкий отдельный клиент Mainline для счётчика узлов
        // (mesh-meter) + снимок узлов для глобуса + кэш bootstrap. ВАЖНО:
        // build() делает СИНХРОННЫЙ DNS дефолтных публичных DHT-роутеров —
        // выполняем его на blocking-пуле (spawn_blocking), поэтому главный поток
        // НЕ блокируется. extra_bootstrap СОХРАНЯЕТ надёжные дефолтные роутеры и
        // ДОБАВЛЯЕТ SmartNet-узел + кэш (не заменяет их — иначе при недоступном
        // SmartNet/пустом резолве DHT не входил в сеть → 0 узлов).
        if dht_on {
            // КЭШ: показываем последний известный снимок DHT МГНОВЕННО (счётчик +
            // узлы для глобуса), пока свежий лёгкий DHT-клиент бутстрапится. Без
            // этого Карта сети/Dashboard мигали 0 на каждом холодном старте.
            let cached = super::load_dht_bootstrap();
            if !cached.is_empty() {
                *super::DHT_PEERS.lock().unwrap() = cached.iter().take(80).cloned().collect();
            }
            super::DHT_NODES.store(
                super::load_dht_nodes_count(),
                std::sync::atomic::Ordering::Relaxed,
            );
            tokio::spawn(async move {
                // Build a fresh lightweight DHT client from default routers +
                // SmartNet seeder + cached/peer-supplied bootstrap nodes.
                async fn build_dht() -> Option<Dht> {
                    let mut extra: Vec<String> = super::get_seeders_internal();
                    extra.extend(super::load_dht_bootstrap());
                    tokio::task::spawn_blocking(move || {
                        let mut mdb = Dht::builder();
                        mdb.extra_bootstrap(&extra);
                        mdb.build().ok()
                    })
                    .await
                    .ok()
                    .flatten()
                }
                let mut dht = match build_dht().await {
                    Some(d) => d,
                    None => return,
                };
                let _ = dht.bootstrapped().await;
                if let Ok(info) = dht.info().await {
                    crate::syslog::log(
                        "dht",
                        format!("bootstrapped — routing table size: {}", info.routing_table_size()),
                    );
                }
                let mut tick: u32 = 0;
                let mut last_logged: u64 = 0;
                // ANTI-CENSORSHIP RE-BOOTSTRAP: if the public routers/seeder are
                // blocked, the table stays empty. After ~60s empty, if peers have
                // supplied fresh bootstrap nodes (via tracker/gossip), rebuild the
                // client with them so we can still join Mainline.
                let mut empty_secs: u64 = 0;
                let mut boot_len_at_build = super::load_dht_bootstrap().len();
                loop {
                    if let Ok(info) = dht.info().await {
                        let size = info.routing_table_size() as u64;
                        if size > 0 {
                            empty_secs = 0;
                            super::DHT_NODES
                                .store(size, std::sync::atomic::Ordering::Relaxed);
                            super::save_dht_nodes_count(size);
                            if last_logged == 0 || size.abs_diff(last_logged) >= 4 {
                                crate::syslog::log("dht", format!("routing table size: {size}"));
                                last_logged = size;
                            }
                        } else {
                            empty_secs = empty_secs.saturating_add(12);
                        }
                    } else {
                        empty_secs = empty_secs.saturating_add(12);
                    }
                    if empty_secs >= 60 {
                        let boot_len = super::load_dht_bootstrap().len();
                        if boot_len > boot_len_at_build {
                            crate::syslog::log(
                                "dht",
                                format!(
                                    "DHT empty — re-bootstrapping with {boot_len} peer-supplied nodes"
                                ),
                            );
                            if let Some(d) = build_dht().await {
                                dht = d;
                                let _ = dht.bootstrapped().await;
                                boot_len_at_build = boot_len;
                                empty_secs = 0;
                            }
                        }
                    }
                    if let Ok(nodes) = dht.to_bootstrap().await {
                        if !nodes.is_empty() {
                            *super::DHT_PEERS.lock().unwrap() =
                                nodes.iter().take(80).cloned().collect();
                            tick = tick.wrapping_add(1);
                            if tick % 10 == 0 {
                                super::save_dht_bootstrap(&nodes);
                            }
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(12)).await;
                }
            });
        }
        let blobs = BlobsProtocol::new(&store, None);
        // Phase 2 chat transit: spawn the gossip protocol on the same endpoint and
        // accept its ALPN so free (unpaid) E2EE messages route over the mesh.
        // ВАЖНО: max_message_size НЕ трогаем — дефолт 4096, ровно как в
        // v1.135.50 (последняя версия с идеальным мешем). Поднятие до 64КБ
        // в v1.135.55 позволило 44КБ-конвертам изображений ходить через
        // gossip и разваливало соседские линки (регрессия v1.135.51-63).
        // Оверсайз в меш не попадает: MAX_PACKET (3300) отсекает на отправке,
        // крупное едет только прямым каналом chat-dm / iroh-blobs.
        let gossip = iroh_gossip::Gossip::builder()
            .spawn(endpoint.clone());
        // Mycelium (mushroom, NIP-1) живёт на ОТДЕЛЬНОМ endpoint'е (свой сокет, ключ, gossip, очередь
        // реле) и поднимается в фоне после узла — на кошелёк, узлы sth и чат не влияет.
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .accept(iroh_gossip::ALPN, gossip.clone())
            // прямая доставка чат-сообщений (второй независимый путь):
            // v2 — тёплый мультиплекс-канал, v1 — legacy одноразовый
            .accept(CHAT_DM_ALPN, ChatDirect)
            .accept(CHAT_DM_ALPN_V2, ChatDirectV2)
            // v1.135.229: прямая анти-энтропия чата (pull недостающих конвертов)
            .accept(crate::chat::sync::CHAT_SYNC_ALPN, crate::chat::sync::ChatSync)
            .accept(crate::chat::voice::VOICE_ALPN, crate::chat::voice::VoiceHandler)
            .spawn();
        crate::chat::gossip::install(app.clone(), gossip.clone());
        crate::social::net::install(app.clone(), gossip.clone());
        // Фаза 3: P2P-шеринг кэша переводов (GPU→CPU) — общие топики `tr:<lang>`.
        crate::translator::tr_net::install(app.clone(), gossip);
        // dApp identity-beacon (мост IP:port → EndpointId для трекерлесс-DHT).
        start_dapp_beacon(app).await;
        let node_id = endpoint.id().to_string();
        // keep the router alive for the process lifetime
        std::mem::forget(P2p {
            endpoint: endpoint.clone(),
            store: store.clone(),
            node_id: node_id.clone(),
            _router: router,
        });
        let handle = (node_id, store, endpoint);
        *P2P.lock().unwrap() = Some(handle.clone());
        crate::mycel::boot(app.clone());
        crate::netstats::spawn_sampler();
        // Прогрев выделенного медиа-endpoint'а мессенджера в фоне (не блокирует
        // старт): к моменту первой отправки/докачки медиа канал уже поднят.
        {
            let app_media = app.clone();
            tokio::spawn(async move {
                let _ = ensure_media(&app_media).await;
            });
        }
        Ok(handle)
    }

    pub async fn node_id(app: &AppHandle) -> Result<String, String> {
        Ok(ensure(app).await?.0)
    }

    /// Build a REPRODUCIBLE tar of `src_dir` into `tar_path`. Files are walked
    /// recursively, sorted by relative path, and written with normalized headers
    /// (mtime/uid/gid/mode fixed). Identical contents therefore yield byte-identical
    /// archives — and thus the SAME iroh blob hash — on every re-seed and on every
    /// machine. This was the root cause of "Unable to download <hash>": `read_dir`
    /// returns entries in an unspecified order, so the old `append_dir_all` produced
    /// a different hash on each re-seed; the announced hash then no longer matched a
    /// blob the provider still had (the previous one got garbage-collected).
    fn build_deterministic_tar(src_dir: &Path, tar_path: &Path) -> Result<(), String> {
        // Windows: canonicalize даёт verbatim-путь `\\?\C:\…` — снимает лимит
        // MAX_PATH (260) для глубоких сайтов-зеркал с длинными именами ассетов.
        // Иначе File::open падал на длинном пути → tar «сбоил» → dApp выпадал из
        // keep-set → GC стирал блоб, а трекер продолжал анонсировать его хэш.
        let canon = std::fs::canonicalize(src_dir).unwrap_or_else(|_| src_dir.to_path_buf());
        let src_dir: &Path = &canon;
        fn walk(
            base: &Path,
            dir: &Path,
            out: &mut Vec<(String, std::path::PathBuf)>,
        ) -> Result<(), String> {
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    walk(base, &p, out)?;
                } else if p.is_file() {
                    let rel = p
                        .strip_prefix(base)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((rel, p));
                }
            }
            Ok(())
        }
        let mut files: Vec<(String, std::path::PathBuf)> = Vec::new();
        walk(src_dir, src_dir, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let f = std::fs::File::create(tar_path).map_err(|e| e.to_string())?;
        let mut b = tar::Builder::new(f);
        b.mode(tar::HeaderMode::Deterministic);
        for (rel, abs) in &files {
            let len = std::fs::metadata(abs).map_err(|e| e.to_string())?.len();
            let mut header = tar::Header::new_gnu();
            header.set_size(len);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            let mut file = std::fs::File::open(abs).map_err(|e| e.to_string())?;
            b.append_data(&mut header, rel, &mut file)
                .map_err(|e| e.to_string())?;
        }
        b.finish().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Удалить из `blobs/temp` файлы старше 24 ч (осиротевшие временные части).
    fn purge_stale_temp(temp_dir: &std::path::Path) {
        let Ok(rd) = std::fs::read_dir(temp_dir) else { return };
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
        let (mut n, mut bytes) = (0u64, 0u64);
        for e in rd.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if !md.is_file() {
                continue;
            }
            let old = md.modified().map(|m| m < cutoff).unwrap_or(true);
            if old && std::fs::remove_file(&p).is_ok() {
                n += 1;
                bytes += md.len();
            }
        }
        if n > 0 {
            crate::syslog::log(
                "net",
                format!("blobs/temp: удалено {} осиротевших временных файлов ({} МБ)", n, bytes / 1_048_576),
            );
        }
    }

    /// Импорт файла в blob-store БЕЗ постоянного тега.
    ///
    /// КРИТИЧНО: `add_path(..).await` в iroh-blobs ≥0.9x = `with_tag()` — создаёт
    /// постоянный auto-тег, а теги — корни GC. Так каждая пересборка tar dApp /
    /// файла / логотипа оставалась на диске навсегда (наблюдали 173 ГБ в `blobs/`).
    /// Здесь блоб живёт только через `SEED_HASHES` (наш keep-set): temp-тег держит
    /// его до вставки хэша в keep-set, дальше защита — keep-set, GC чистит остальное.
    async fn import_path(store: &FsStore, abs: std::path::PathBuf) -> Result<Hash, String> {
        let tt = store.blobs().add_path(abs).temp_tag().await.map_err(|e| e.to_string())?;
        let hh = tt.hash();
        if let Some(set) = SEED_HASHES.lock().unwrap().as_mut() {
            set.insert(hh);
        }
        drop(tt);
        Ok(hh)
    }

    /// Разовая миграция: снять все постоянные теги (наследие `add_path().await`),
    /// чтобы GC смог освободить осиротевшие блобы. Источник истины — `SEED_HASHES`.
    async fn drop_legacy_tags(store: &FsStore) {
        match store.tags().delete_all().await {
            Ok(n) if n > 0 => crate::syslog::log(
                "net",
                format!("blobs: снято {} постоянных тегов — GC освободит старые версии", n),
            ),
            Ok(_) => {}
            Err(e) => crate::syslog::log("net", format!("blobs: не удалось снять теги: {}", e)),
        }
    }

    /// Seed every local dApp folder into the blob store as a single tar blob
    /// (one Raw blob per app — avoids HashSeq/collection edge cases). Returns
    /// (app_id, hash). The tar archives live under data/blobs/seed/<id>.tar.
    pub async fn seed_all(app: &AppHandle) -> Result<Vec<(String, String)>, String> {
        let (_, store, _) = ensure(app).await?;
        let data = ensure_data_dir(app);
        let dapps = data.join("dapps");
        let seed_dir = data.join("blobs").join("seed");
        let _ = std::fs::create_dir_all(&seed_dir);
        let mut out = Vec::new();
        // Предыдущие хэши (для отката при сбое пересборки tar одного dApp).
        let prev: HashMap<String, String> = super::SEEDED_APPS.lock().unwrap().clone();
        // Сбой tar/импорта ОДНОГО dApp: раньше приложение просто выпадало из
        // keep-set → GC стирал его блоб, а TRACKER_SNAPSHOT продолжал анонсировать
        // старый хэш → у сидеров вечный «blob absent on provider». Теперь, если
        // прежний блоб ещё Complete в сторе, продолжаем раздавать ЕГО.
        let keep_prev = |id: &str, store: &FsStore| {
            let store = store.clone();
            let id = id.to_string();
            let prev_hash = prev.get(&id).cloned();
            async move {
                if let Some(ph) = prev_hash {
                    if let Ok(h) = ph.parse::<Hash>() {
                        if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(h).await {
                            eprintln!("seed_all: {} — keeping previous blob {}…", id, &ph[..ph.len().min(12)]);
                            return Some((id, ph));
                        }
                    }
                }
                None
            }
        };
        if let Ok(rd) = std::fs::read_dir(&dapps) {
            for e in rd.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let id = e.file_name().to_string_lossy().to_string();
                let tar_path = seed_dir.join(format!("{}.tar", id));
                // (re)build a REPRODUCIBLE archive from the current app contents so
                // the iroh hash is stable across re-seeds. CPU/IO heavy → spawn_blocking
                // (UI thread never stalls). УСТОЙЧИВОСТЬ: сбой/огромный размер ОДНОГО
                // приложения НЕ должен срывать сидинг остальных — логируем и пропускаем,
                // иначе одно тяжёлое приложение блокирует обновление всей директории.
                let src = e.path();
                let tar_clone = tar_path.clone();
                match tauri::async_runtime::spawn_blocking(move || build_deterministic_tar(&src, &tar_clone)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        eprintln!("seed_all: tar build failed for {} — {}", id, err);
                        if let Some(kept) = keep_prev(&id, &store).await {
                            out.push(kept);
                        }
                        continue;
                    }
                    Err(err) => {
                        eprintln!("seed_all: tar task join failed for {} — {}", id, err);
                        if let Some(kept) = keep_prev(&id, &store).await {
                            out.push(kept);
                        }
                        continue;
                    }
                }
                let abs = match std::path::absolute(&tar_path) {
                    Ok(a) => a,
                    Err(err) => {
                        eprintln!("seed_all: abs path failed for {} — skipping: {}", id, err);
                        continue;
                    }
                };
                let hh = match import_path(&store, abs).await {
                    Ok(h) => h,
                    Err(err) => {
                        eprintln!("seed_all: iroh import failed for {} — (large app / disk?): {}", id, err);
                        if let Some(kept) = keep_prev(&id, &store).await {
                            out.push(kept);
                        }
                        continue;
                    }
                };
                // Only advertise a blob we can actually serve. Guards against
                // announcing a partial/unreadable store entry.
                match store.blobs().status(hh).await {
                    Ok(BlobStatus::Complete { .. }) => out.push((id, hh.to_string())),
                    _ => {
                        eprintln!("seed_all: {} blob not Complete after import — not announced", id);
                        if let Some(kept) = keep_prev(&id, &store).await {
                            out.push(kept);
                        }
                    }
                }
            }
        }
        // Полный набор раздаваемых dApp (включая бесплатные «только по ссылке»)
        // — источник hash'а для прямых ссылок; обновляется при каждом ре-сиде.
        *super::SEEDED_APPS.lock().unwrap() = out.iter().cloned().collect();
        // Синхронизируем хэши в трекерном анонсе с тем, что РЕАЛЬНО раздаём:
        // snapshot строился один раз в start_discovery, а seed_all зовётся и из
        // delete/optimize/share — без этого трекер получал устаревшие хэши.
        if let Some(snap) = super::TRACKER_SNAPSHOT.lock().unwrap().as_mut() {
            let now: HashMap<&String, &String> = out.iter().map(|(i, h)| (i, h)).collect();
            for a in snap.apps.iter_mut() {
                if let Some(h) = now.get(&a.id) {
                    if a.hash != **h {
                        a.hash = (*h).clone();
                    }
                }
            }
        }
        // PNG-логотипы и DAPP Preview (440×180) — каждый отдельным маленьким
        // iroh-блобом; хэши анонсируются TXT-записями `l{i}`/`p{i}` — каталог у
        // других пиров показывает картинки ДО установки. Logo: только PNG;
        // preview: PNG/JPG; оба ≤ 256 KB; приватные dApp пропускаем
        // (не светим визуальную identity наружу).
        let mut logo_out: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut preview_out: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (id, _) in &out {
            if crate::private_dapp::is_private(id) {
                continue;
            }
            let dir = dapps.join(id);
            let lnames = [
                format!("{}-logo.png", id),
                "logo.png".to_string(),
                "logo.PNG".to_string(),
                "icon.png".to_string(),
            ];
            if let Some(path) = lnames.iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
                let valid = std::fs::read(&path)
                    .map(|b| b.len() <= super::LOGO_MAX_BYTES && b.starts_with(&super::PNG_MAGIC))
                    .unwrap_or(false);
                if valid {
                    if let Ok(abs) = std::path::absolute(&path) {
                        if let Ok(hh) = import_path(&store, abs).await {
                            if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(hh).await {
                                logo_out.insert(id.clone(), hh.to_string());
                            }
                        }
                    }
                }
            }
            let pnames = [format!("{}-preview.png", id), format!("{}-preview.jpg", id)];
            if let Some(path) = pnames.iter().map(|n| dir.join(n)).find(|p| p.is_file()) {
                let valid = std::fs::read(&path)
                    .map(|b| {
                        b.len() <= super::LOGO_MAX_BYTES
                            && (b.starts_with(&super::PNG_MAGIC)
                                || b.starts_with(&super::JPEG_MAGIC))
                    })
                    .unwrap_or(false);
                if valid {
                    if let Ok(abs) = std::path::absolute(&path) {
                        if let Ok(hh) = import_path(&store, abs).await {
                            if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(hh).await {
                                preview_out.insert(id.clone(), hh.to_string());
                            }
                        }
                    }
                }
            }
        }
        *super::SEEDED_LOGOS.lock().unwrap() = logo_out.clone();
        *super::SEEDED_PREVIEWS.lock().unwrap() = preview_out.clone();
        // also seed File Exchange files (v2.0 Stage 1): add each to the store and
        // protect its blob alongside dApp blobs so GC never reclaims a live file.
        let mut file_hashes: Vec<Hash> = Vec::new();
        for fp in crate::files::seeding_file_paths(app) {
            if let Ok(abs) = std::path::absolute(&fp) {
                if let Ok(hh) = import_path(&store, abs).await {
                    if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(hh).await {
                        file_hashes.push(hh);
                    }
                }
            }
        }
        // Мессенджер: ре-сид блобов свежих изображений чата (кэш data/messenger,
        // ≤7 дней, cap 100) — участники остаются провайдерами картинок для остальных.
        for fp in crate::chat::media::seed_paths() {
            if let Ok(abs) = std::path::absolute(&fp) {
                if let Ok(hh) = import_path(&store, abs).await {
                    if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(hh).await {
                        file_hashes.push(hh);
                    }
                }
            }
        }
        // refresh the GC keep-set: only these (current) blobs are protected.
        let mut keep: HashSet<Hash> = out.iter().filter_map(|(_, h)| h.parse::<Hash>().ok()).collect();
        keep.extend(file_hashes);
        // блобы логотипов/preview тоже защищаем от GC — иначе пропадут между ре-сидами
        keep.extend(logo_out.values().filter_map(|h| h.parse::<Hash>().ok()));
        keep.extend(preview_out.values().filter_map(|h| h.parse::<Hash>().ok()));
        *SEED_HASHES.lock().unwrap() = Some(keep);

        // Трекерлесс-анонс раздаваемых dApp в Mainline DHT (BEP5) + обновление
        // served-набора identity-beacon. За тумблером cfg:dht_providers_enabled.
        if super::dht_providers_enabled_internal() {
            let port = *DAPP_BEACON_PORT.lock().unwrap();
            if let Some(port) = port {
                let mut served: std::collections::HashSet<[u8; 20]> = std::collections::HashSet::new();
                for (id, h) in &out {
                    let ih = crate::dht_dapp::app_infohash(id);
                    served.insert(ih);
                    crate::torrent::dht_announce_dapp(ih, port);
                    // v2-ответ бикона: хэш tar-блоба dApp — клиент по прямой
                    // ссылке / DHT-резолву ставит приложение без трекеров.
                    if let Ok(hh) = h.parse::<Hash>() {
                        DEV_EXTRA.lock().unwrap().insert(ih, *hh.as_bytes());
                    }
                }
                // DevHub: анонсируем себя как сидера собственного подписанного
                // индекса dev://-репозиториев (обнаружение по адресу автора).
                // Индекс сидируется как iroh-blob, его хэш регистрируется в
                // биконе (v2-ответ) — читатель получает всё за 1 UDP-раундтрип.
                if let Some(addr) = crate::devhub::local_devhub_address(app) {
                    let ih = crate::dht_dapp::dev_infohash(&addr);
                    served.insert(ih);
                    crate::torrent::dht_announce_dapp(ih, port);
                    let ipath = crate::devhub::index_path_for(app, &addr);
                    if let Ok(hash) = seed_file(app, &ipath.to_string_lossy()).await {
                        if let Ok(h) = hash.parse::<Hash>() {
                            DEV_EXTRA.lock().unwrap().insert(ih, *h.as_bytes());
                        }
                    }
                }
                // E2EE-альбомы: реанонс keyfile'ов всех своих платных альбомов
                // (покупатели получают ключи, пока владелец онлайн).
                for kv in crate::DB.scan_prefix(b"social:albumkeysblob:").flatten() {
                    let id = String::from_utf8_lossy(&kv.0)
                        .trim_start_matches("social:albumkeysblob:")
                        .to_string();
                    let hash = String::from_utf8_lossy(&kv.1).to_string();
                    if id.is_empty() || hash.is_empty() {
                        continue;
                    }
                    let ih = crate::dht_dapp::album_infohash(&id);
                    served.insert(ih);
                    crate::torrent::dht_announce_dapp(ih, port);
                    if let Ok(h) = hash.parse::<Hash>() {
                        DEV_EXTRA.lock().unwrap().insert(ih, *h.as_bytes());
                    }
                    // ре-сид самого keyfile'а (после рестарта iroh-store пуст)
                    let kf = ensure_data_dir(app)
                        .join("social_cache")
                        .join(format!("albumkeys-{}.json", id));
                    if kf.exists() {
                        let _ = seed_file(app, &kf.to_string_lossy()).await;
                    }
                }
                if !served.is_empty() {
                    *DAPP_SERVED.lock().unwrap() = served;
                    crate::syslog::log(
                        "dht",
                        format!("announced {} dApps to Mainline DHT (beacon :{})", out.len(), port),
                    );
                }
            }
        }
        Ok(out)
    }

    /// Seed a single File Exchange file into the iroh store and add its blob to
    /// the live GC keep-set (so it survives until explicitly un-seeded/deleted).
    /// Returns the content hash string. Used right after `add_file`.
    pub async fn seed_file(app: &AppHandle, path: &str) -> Result<String, String> {
        let (_, store, _) = ensure(app).await?;
        let abs = std::path::absolute(path).map_err(|e| e.to_string())?;
        let hh = import_path(&store, abs).await?;
        Ok(hh.to_string())
    }

    /// Download a File Exchange blob `hash` from `node_id` (with headless-seeder
    /// fallback) and export the raw bytes to `dest`. Used by `download_file`.
    pub async fn fetch_file(
        app: &AppHandle,
        node_id: &str,
        hash: &str,
        dest: &str,
        extra_providers: &[String],
    ) -> Result<String, String> {
        let (_, store, endpoint) = ensure(app).await?;
        let h: Hash = hash.parse().map_err(|_| "bad hash".to_string())?;
        // DHT provider discovery: race the download across EVERY known provider
        // instead of a single fixed seeder. iroh resolves each EndpointId's LIVE
        // address via its DHT/pkarr discovery, so an offline `seeder_node` no longer
        // blocks the transfer as long as ANY provider currently seeds the content.
        let mut ids: Vec<EndpointId> = Vec::new();
        fn push_id(s: &str, ids: &mut Vec<EndpointId>) {
            if let Ok(e) = s.trim().parse::<EndpointId>() {
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
        }
        push_id(node_id, &mut ids);
        for p in extra_providers {
            push_id(p, &mut ids);
        }
        // headless / tracker-known seeders (WAN fallback for the same content hash)
        let headless = tokio::task::spawn_blocking(|| {
            super::network_map_blocking()
                .into_iter()
                .filter(|s| !s.metrics_url.trim().is_empty())
                .map(|s| s.endpoint_id)
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for hid in headless {
            push_id(&hid, &mut ids);
        }
        if ids.is_empty() {
            return Err("no known providers for this file".to_string());
        }
        crate::syslog::log(
            "p2p",
            format!(
                "fetch {}: trying {} provider(s) via DHT",
                &hash[..hash.len().min(8)],
                ids.len()
            ),
        );
        // Live progress: РАНЬШЕ здесь был observe()-стрим, но его watcher в
        // iroh-blobs 0.103 ПАНИКУЕТ в потоке стора («poisoned storage should
        // not be used», bao_file.rs:410), когда entry персистится в момент
        // завершения докачки — актор стора умирал, и ВСЕ blob-операции
        // зависали до перезапуска приложения. Безопасный поллинг status()
        // раз в 500мс не трогает watcher-механику вовсе.
        {
            let store_obs = store.clone();
            let hs = hash.to_string();
            tauri::async_runtime::spawn(async move {
                let mut last: u64 = 0;
                let mut stalled: u32 = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    match store_obs.blobs().status(h).await {
                        Ok(BlobStatus::Partial { size }) => {
                            let done = size.unwrap_or(0);
                            if done == last {
                                stalled += 1;
                                // байты не растут 5 минут — загрузка брошена
                                if stalled > 600 {
                                    break;
                                }
                            } else {
                                stalled = 0;
                                last = done;
                            }
                            if let Ok(mut map) = DL_BYTES.lock() {
                                map.insert(hs.clone(), done);
                            }
                        }
                        Ok(BlobStatus::NotFound) => {
                            // блоб ещё не начался (идёт поиск пиров) — ждём,
                            // иначе прогресс-таск умирает ДО первых байтов
                            stalled += 1;
                            if stalled > 600 {
                                break;
                            }
                        }
                        _ => break, // Complete / ошибка — прогресс не нужен
                    }
                }
                if let Ok(mut map) = DL_BYTES.lock() {
                    map.remove(&hs);
                }
            });
        }
        download_complete_any(&store, &endpoint, h, ids).await?;
        if let Some(parent) = std::path::Path::new(dest).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let dest_path = std::path::PathBuf::from(dest);
        let _ = std::fs::remove_file(&dest_path);
        store.blobs().export(h, &dest_path).await.map_err(|e| e.to_string())?;
        // keep seeding what we just fetched (help the swarm) + protect from GC
        if let Some(set) = SEED_HASHES.lock().unwrap().as_mut() {
            set.insert(h);
        }
        Ok(hash.to_string())
    }

    // =====================================================================
    //  МЕДИА-ЭНДПОИНТ мессенджера: ОТДЕЛЬНЫЙ QUIC-сокет только для blobs
    //  (фото/аудио чата). Передача медиа физически не может конкурировать с
    //  основным endpoint'ом (gossip + chat-dm) за сокет/линк — текстовые
    //  сообщения всегда мгновенны, сколько бы медиа ни качалось.
    // =====================================================================

    static MEDIA_EP: Lazy<tokio::sync::Mutex<Option<Endpoint>>> =
        Lazy::new(|| tokio::sync::Mutex::new(None));

    /// Поднять (однократно) выделенный медиа-endpoint: своя identity
    /// (data/media.key), свой UDP-сокет, ОБЩИЙ blob-store с основным узлом.
    /// Принимает только iroh-blobs ALPN. Настройки сети (relay-only, DHT,
    /// стелс, мобильные фолбэки) зеркалируют основной endpoint.
    /// Отдельный endpoint со своим ключом `<data>/<key_file>` и теми же настройками сети, что у
    /// основного (стелс/n1-реле/DHT/mDNS): медиа мессенджера и Mycelium не делят сокет и очередь
    /// реле с кошельком и чатом.
    pub(super) async fn isolated_endpoint(app: &AppHandle, key_file: &str) -> Result<Endpoint, String> {
        let secret = {
            let key_path = ensure_data_dir(app).join(key_file);
            match tokio::fs::read(&key_path).await {
                Ok(b) if b.len() == 32 => {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&b);
                    iroh::SecretKey::from_bytes(&arr)
                }
                _ => {
                    let sk = iroh::SecretKey::generate();
                    let _ = tokio::fs::write(&key_path, sk.to_bytes()).await;
                    sk
                }
            }
        };
        let relay_only = super::relay_only_internal();
        let dht_on = super::dht_enabled_internal();
        let dht_boot_ips: Vec<String> =
            if dht_on { resolve_dht_bootstrap().await } else { Vec::new() };
        let mobile_fb = mobile_fallbacks_active().await;
        let stealth = crate::stealth::stealth_enabled_persisted();
        let n1 = super::get_relay_config();
        let endpoint = if stealth && !n1.urls.is_empty() {
            let sb = secret.to_bytes();
            match build_stealth_endpoint(secret, dht_on, &dht_boot_ips, &n1.urls, mobile_fb).await
            {
                Ok(ep) => ep,
                Err(_) => {
                    let fb = iroh::SecretKey::from_bytes(&sb);
                    build_default_endpoint(fb, relay_only, dht_on, &dht_boot_ips, mobile_fb)
                        .await?
                }
            }
        } else {
            build_default_endpoint(secret, relay_only, dht_on, &dht_boot_ips, mobile_fb).await?
        };
        if !stealth {
            apply_smartnet_relays(&endpoint).await;
        }
        Ok(endpoint)
    }

    pub(super) fn media_endpoint_now() -> Option<Endpoint> {
        MEDIA_EP.try_lock().ok().and_then(|g| g.clone())
    }

    async fn ensure_media(app: &AppHandle) -> Result<Endpoint, String> {
        // ВАЖНО: НЕ вызываем ensure() здесь — ensure() спавнит ensure_media
        // (прогрев), и взаимный await создаёт цикл Send-проверки компилятора
        // (E0698-подобный «future cannot be sent between threads»). Основной
        // узел к этому моменту уже поднят — берём общий store из P2P напрямую.
        let store = match P2P.lock().unwrap().as_ref() {
            Some((_, s, _)) => s.clone(),
            None => return Err("main endpoint is not up yet".to_string()),
        };
        let mut guard = MEDIA_EP.lock().await;
        if let Some(ep) = guard.as_ref() {
            return Ok(ep.clone());
        }
        let endpoint = isolated_endpoint(app, "media.key").await?;
        let blobs = BlobsProtocol::new(&store, None);
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();
        std::mem::forget(router); // живёт весь процесс
        crate::syslog::log(
            "p2p",
            format!(
                "media endpoint bound: {} (isolated messenger media channel)",
                endpoint.id()
            ),
        );
        *guard = Some(endpoint.clone());
        Ok(endpoint)
    }

    /// EndpointId выделенного медиа-канала (поле `n` медиа-конвертов чата).
    pub async fn media_node_id(app: &AppHandle) -> Result<String, String> {
        let _ = ensure(app).await?; // гарантируем основной узел (store)
        Ok(ensure_media(app).await?.id().to_string())
    }

    /// Скачивание медиа мессенджера через ВЫДЕЛЕННЫЙ endpoint: докачка не
    /// конкурирует с gossip/chat-dm за сокет. Фолбэк — основной endpoint
    /// (если медиа-канал не поднялся).
    pub async fn fetch_media(
        app: &AppHandle,
        node_id: &str,
        hash: &str,
        dest: &str,
        extra_providers: &[String],
    ) -> Result<String, String> {
        let (_, store, main_ep) = ensure(app).await?;
        let endpoint = match ensure_media(app).await {
            Ok(ep) => ep,
            Err(_) => main_ep,
        };
        let h: Hash = hash.parse().map_err(|_| "bad hash".to_string())?;
        let mut ids: Vec<EndpointId> = Vec::new();
        for s in std::iter::once(node_id).chain(extra_providers.iter().map(|s| s.as_str())) {
            if let Ok(e) = s.trim().parse::<EndpointId>() {
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
        }
        if ids.is_empty() {
            return Err("no known providers for this file".to_string());
        }
        // живой прогресс загрузки (UI-бар): безопасный поллинг status() —
        // observe()-watcher паникует в потоке стора при завершении докачки
        // (см. комментарий в fetch_file выше)
        {
            let store_obs = store.clone();
            let hs = hash.to_string();
            tauri::async_runtime::spawn(async move {
                let mut last: u64 = 0;
                let mut stalled: u32 = 0;
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    match store_obs.blobs().status(h).await {
                        Ok(BlobStatus::Partial { size }) => {
                            let done = size.unwrap_or(0);
                            if done == last {
                                stalled += 1;
                                if stalled > 600 {
                                    break;
                                }
                            } else {
                                stalled = 0;
                                last = done;
                            }
                            if let Ok(mut map) = DL_BYTES.lock() {
                                map.insert(hs.clone(), done);
                            }
                        }
                        Ok(BlobStatus::NotFound) => {
                            // блоб ещё не начался (идёт поиск пиров) — ждём
                            stalled += 1;
                            if stalled > 600 {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                if let Ok(mut map) = DL_BYTES.lock() {
                    map.remove(&hs);
                }
            });
        }
        download_complete_any(&store, &endpoint, h, ids).await?;
        if let Some(parent) = std::path::Path::new(dest).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let dest_path = std::path::PathBuf::from(dest);
        let _ = std::fs::remove_file(&dest_path);
        store.blobs().export(h, &dest_path).await.map_err(|e| e.to_string())?;
        if let Some(set) = SEED_HASHES.lock().unwrap().as_mut() {
            set.insert(h);
        }
        Ok(hash.to_string())
    }

    /// Скачать блоб PNG-логотипа dApp у любого из провайдеров и экспортировать
    /// в файл кэша `dest`. Малый блоб (≤ 256 KB) — без прогресса и keep-set.
    pub async fn fetch_logo(
        app: &AppHandle,
        hash: &str,
        node_ids: &[String],
        dest: &std::path::Path,
    ) -> Result<(), String> {
        let (_, store, endpoint) = ensure(app).await?;
        let h: Hash = hash.parse().map_err(|_| "bad hash".to_string())?;
        let mut ids: Vec<EndpointId> = Vec::new();
        for s in node_ids {
            if let Ok(e) = s.trim().parse::<EndpointId>() {
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
        }
        if ids.is_empty() {
            return Err("no providers".to_string());
        }
        download_complete_any(&store, &endpoint, h, ids).await?;
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let dest_path = dest.to_path_buf();
        let _ = std::fs::remove_file(&dest_path);
        store
            .blobs()
            .export(h, &dest_path)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Live download progress for a blob: (bytes_on_disk_so_far, complete?).
    /// Reads the iroh blob-store status; Partial reports the bytes transferred.
    pub async fn blob_progress(app: &AppHandle, hash: &str) -> (u64, bool) {
        // live byte counter from the concurrent observe task (accurate mid-transfer)
        let live = DL_BYTES.lock().ok().and_then(|m| m.get(hash).copied());
        let (_, store, _) = match ensure(app).await {
            Ok(t) => t,
            Err(_) => return (live.unwrap_or(0), false),
        };
        let h: Hash = match hash.parse() {
            Ok(h) => h,
            Err(_) => return (live.unwrap_or(0), false),
        };
        match store.blobs().status(h).await {
            Ok(BlobStatus::Complete { .. }) => (0, true),
            Ok(BlobStatus::Partial { size }) => (live.or(size).unwrap_or(0), false),
            _ => (live.unwrap_or(0), false),
        }
    }

    /// Total bytes the iroh blob store occupies on disk (data/blobs).
    pub fn blob_store_bytes(app: &AppHandle) -> u64 {
        super::dir_size(&ensure_data_dir(app).join("blobs"))
    }

    /// Download blob `h` from provider `nid`, RETRYING until the local store
    /// reports it COMPLETE. iroh-blobs 0.103 `download().await` resolves to Ok even
    /// when nothing transferred (its `complete()` only fails on Error/DownloadError
    /// and otherwise returns Ok when the provider stream ends), so trusting Ok left
    /// installs silently incomplete and made the retry loop dead code. Gating on the
    /// actual blob status fixes both.
    async fn download_complete(
        store: &FsStore,
        endpoint: &Endpoint,
        h: Hash,
        nid: EndpointId,
    ) -> Result<(), String> {
        // Cross-app dedup: the iroh store is content-addressed, so a blob already
        // pulled/seeded for ANY other app with byte-identical content is reused as
        // is — short-circuit before touching the network (no duplicate transfer).
        if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(h).await {
            return Ok(());
        }
        let mut last_err = String::from("provider served no data");
        for attempt in 0..5u32 {
            match store.downloader(endpoint).download(h, Some(nid)).await {
                Ok(_) => crate::peerbook::remember(endpoint, nid, crate::peerbook::Role::Seed).await,
                Err(e) => last_err = e.to_string(),
            }
            match store.blobs().status(h).await {
                Ok(BlobStatus::Complete { .. }) => return Ok(()),
                Ok(BlobStatus::Partial { size }) => {
                    last_err = format!(
                        "provider served only a partial blob ({} bytes) — relay path likely dropping data",
                        size.unwrap_or(0)
                    );
                }
                Ok(BlobStatus::NotFound) => {
                    last_err =
                        "provider returned no data (blob absent on provider, or refused to serve)"
                            .to_string();
                }
                Err(e) => last_err = format!("status check failed: {}", e),
            }
            if attempt < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            }
        }
        Err(last_err)
    }

    /// Like `download_complete` but races the download across a LIST of candidate
    /// providers (fixed seeder + DB seeders + DHT/tracker-discovered). iroh tries
    /// them and resolves each EndpointId's address via DHT/pkarr; we retry on the
    /// whole set until the blob is COMPLETE locally.
    async fn download_complete_any(
        store: &FsStore,
        endpoint: &Endpoint,
        h: Hash,
        providers: Vec<EndpointId>,
    ) -> Result<(), String> {
        if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(h).await {
            return Ok(());
        }
        if providers.is_empty() {
            return Err("no providers".to_string());
        }
        let mut last_err = String::from("no provider served the blob");
        for attempt in 0..5u32 {
            if let Err(e) = store.downloader(endpoint).download(h, providers.clone()).await {
                last_err = e.to_string();
            }
            match store.blobs().status(h).await {
                Ok(BlobStatus::Complete { .. }) => return Ok(()),
                Ok(BlobStatus::Partial { size }) => {
                    last_err = format!(
                        "провайдеры отдали лишь часть blob ({} байт) — путь через реле теряет данные",
                        size.unwrap_or(0)
                    );
                }
                Ok(BlobStatus::NotFound) => {
                    last_err =
                        "ни один провайдер не отдал данные (blob отсутствует/недоступен)".to_string();
                }
                Err(e) => last_err = format!("status check failed: {}", e),
            }
            if attempt < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            }
        }
        Err(last_err)
    }

    /// Общий на весь процесс бандит выбора пира (Контур 2). Веса подгружаются из
    /// Sled `swarm:weights` при первом обращении и живут между установками, так
    /// что клиент запоминает быстрых/надёжных сидеров.
    pub(super) static SWARM_BANDIT: Lazy<Arc<crate::swarm::SmartSwarmBandit>> = Lazy::new(|| {
        let b = Arc::new(crate::swarm::SmartSwarmBandit::new());
        if let Ok(Some(v)) = crate::DB.get(b"swarm:weights") {
            if let Ok(s) = std::str::from_utf8(&v) {
                b.from_json(s);
            }
        }
        b
    });

    /// Сохранить снимок весов бандита в Sled (после каждой установки).
    fn save_swarm_weights() {
        let _ = crate::DB.insert(b"swarm:weights", SWARM_BANDIT.to_json().as_bytes());
        let _ = crate::DB.flush();
        log_top_seeders_throttled();
    }

    /// Топ-N сидеров по скорости из весов бандита.
    pub fn top_seeders(n: usize) -> Vec<super::SwarmSeeder> {
        let mut v: Vec<super::SwarmSeeder> = SWARM_BANDIT
            .export_state()
            .into_iter()
            .map(|s| super::SwarmSeeder {
                peer: s.peer,
                speed: s.speed,
                errors: s.errors,
                attempts: s.attempts,
                successes: s.successes,
            })
            .collect();
        v.sort_by(|a, b| b.speed.partial_cmp(&a.speed).unwrap_or(std::cmp::Ordering::Equal));
        v.truncate(n);
        v
    }

    /// Anti-spam: пишем в System Console top-3 сидеров НЕ чаще раза в 60с и
    /// только когда есть накопленная статистика (после реальных загрузок).
    static LAST_TOP_LOG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn log_top_seeders_throttled() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last = LAST_TOP_LOG.load(std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(last) < 60 {
            return;
        }
        let top = top_seeders(3);
        if top.iter().all(|s| s.successes == 0) {
            return;
        }
        LAST_TOP_LOG.store(now, std::sync::atomic::Ordering::Relaxed);
        let line = top
            .iter()
            .filter(|s| s.successes > 0)
            .enumerate()
            .map(|(i, s)| {
                let short = if s.peer.len() > 12 {
                    format!("{}…{}", &s.peer[..6], &s.peer[s.peer.len() - 4..])
                } else {
                    s.peer.clone()
                };
                format!("{}. {} {:.0}Б/с", i + 1, short, s.speed)
            })
            .collect::<Vec<_>>()
            .join("  ·  ");
        crate::syslog::log("swarm", format!("🏆 top seeders: {}", line));
    }

    /// Bandit-failover для ОДНОГО blob (текущий формат — один tar-blob на dApp).
    /// ε-greedy выбирает исторически лучшего/быстрого пира, тянет blob до
    /// `Complete` с коротким per-try таймаутом, награждает/штрафует по итогу и
    /// МГНОВЕННО переключается на следующего при зависании. Учится, какие
    /// сидеры надёжны (веса переживают перезапуск). Убирает многосекундные
    /// подвисания на дохлом объявленном пире.
    async fn download_blob_bandit(
        store: &FsStore,
        endpoint: &Endpoint,
        h: Hash,
        providers: &[EndpointId],
        bandit: &crate::swarm::SmartSwarmBandit,
    ) -> Result<(), String> {
        if let Ok(BlobStatus::Complete { .. }) = store.blobs().status(h).await {
            return Ok(());
        }
        if providers.is_empty() {
            return Err("нет провайдеров".to_string());
        }
        let cands: Vec<String> = providers.iter().map(|e| e.to_string()).collect();
        for c in &cands {
            bandit.register_peer(c.clone());
        }
        let per_try = std::time::Duration::from_secs(8);
        let tries = ((cands.len() as u32) * 2).clamp(6, 16);
        let mut last = String::from("ни один провайдер не отдал blob");
        for _ in 0..tries {
            let peer = match bandit.select_peer(&cands, 0.15) {
                Some(p) => p,
                None => break,
            };
            let nid: EndpointId = match peer.parse() {
                Ok(e) => e,
                Err(_) => continue,
            };
            let started = std::time::Instant::now();
            match tokio::time::timeout(per_try, store.downloader(endpoint).download(h, Some(nid)))
                .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => last = "peer timeout".to_string(),
            }
            match store.blobs().status(h).await {
                Ok(BlobStatus::Complete { .. }) => {
                    // один и тот же blob → номинальные байты дают speed ∝ 1/duration
                    // (корректное относительное ранжирование пиров по скорости).
                    bandit.update_peer_reward(&peer, 1_000_000, started.elapsed(), true);
                    return Ok(());
                }
                Ok(BlobStatus::Partial { size }) => {
                    last = format!("частичный blob ({} байт)", size.unwrap_or(0));
                    bandit.update_peer_reward(&peer, 0, std::time::Duration::ZERO, false);
                }
                Ok(BlobStatus::NotFound) => {
                    last = "blob отсутствует у пира".to_string();
                    bandit.update_peer_reward(&peer, 0, std::time::Duration::ZERO, false);
                }
                Err(e) => {
                    last = format!("status: {}", e);
                    bandit.update_peer_reward(&peer, 0, std::time::Duration::ZERO, false);
                }
            }
        }
        Err(last)
    }

    /// Собрать список пиров-кандидатов для blob-`hash`: объявленный `node_id` +
    /// headless/tracker-сидеры из карты сети (тот же контент-хэш).
    /// Набор app_infohash’ей, которые мы сейчас раздаём (для identity-beacon).
    pub(super) static DAPP_SERVED: Lazy<crate::dht_dapp::ServedSet> =        Lazy::new(|| Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())));
    /// infohash → blake3-хэш blob'а подписанного DevHub-индекса (v2-ответ бикона).
    pub(super) static DEV_EXTRA: Lazy<crate::dht_dapp::ExtraMap> =
        Lazy::new(|| Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())));
    /// UDP-порт поднятого identity-beacon (его анонсируем в DHT).
    pub(super) static DAPP_BEACON_PORT: Lazy<std::sync::Mutex<Option<u16>>> =
        Lazy::new(|| std::sync::Mutex::new(None));

    /// Диагностика identity-beacon для UI: (UDP-порт бикона, число анонсируемых dApp).
    pub(super) fn beacon_diag() -> (Option<u16>, u32) {
        let port = *DAPP_BEACON_PORT.lock().unwrap();
        let announced = DAPP_SERVED.lock().unwrap().len() as u32;
        (port, announced)
    }

    /// Немедленный анонс DevHub-автора: infohash его подписанного индекса
    /// добавляется в served-набор бикона + объявляется в Mainline DHT.
    /// Вызывается сразу при создании/обновлении репозитория (не ждём цикла
    /// seed_all). Идемпотентно.
    pub(super) fn announce_dev(address: &str) {
        let ih = crate::dht_dapp::dev_infohash(address);
        DAPP_SERVED.lock().unwrap().insert(ih);
        if let Some(port) = *DAPP_BEACON_PORT.lock().unwrap() {
            crate::torrent::dht_announce_dapp(ih, port);
        }
    }

    /// Зарегистрировать blake3-хэш blob'а СВОЕГО подписанного индекса для
    /// v2-ответа бикона (читатели получают хэш прямо в UDP-ответе).
    pub(super) fn register_dev_index_hash(address: &str, hash_hex: &str) {
        if let Ok(h) = hash_hex.parse::<Hash>() {
            let ih = crate::dht_dapp::dev_infohash(address);
            DEV_EXTRA.lock().unwrap().insert(ih, *h.as_bytes());
        }
    }

    /// blake3-хэш blob'а СВОЕГО подписанного индекса (для gossip-анонса DevHub).
    pub(super) fn dev_index_hash(address: &str) -> Option<String> {
        let ih = crate::dht_dapp::dev_infohash(address);
        DEV_EXTRA
            .lock()
            .unwrap()
            .get(&ih)
            .map(|b| Hash::from_bytes(*b).to_string())
    }

    /// DevHub-обнаружение: BEP5 get_peers(dev_infohash) → опрос биконов →
    /// (проверенные EndpointId-провайдеры, хэш индекса из v2-ответа, живые сидеры).
    /// Pull-based: 1 DHT-запрос + до 12 UDP-пакетов, сеть не флудится.
    pub(super) async fn dev_discover(address: &str) -> (Vec<String>, Option<String>, u32) {
        let ih = crate::dht_dapp::dev_infohash(address);
        let cache_key = format!("dht:providers:dev:{}", address);
        discover_extra(ih, &cache_key).await
    }

    /// Анонс произвольного infohash с extra-хэшем в v2-ответе бикона
    /// (keyfile платного альбома и т.п.).
    pub(super) fn announce_extra(ih: [u8; 20], hash_hex: &str) {
        if let Ok(h) = hash_hex.parse::<Hash>() {
            DEV_EXTRA.lock().unwrap().insert(ih, *h.as_bytes());
        }
        DAPP_SERVED.lock().unwrap().insert(ih);
        if let Some(port) = *DAPP_BEACON_PORT.lock().unwrap() {
            crate::torrent::dht_announce_dapp(ih, port);
        }
    }

    /// Общий discovery: BEP5 get_peers(ih) → опрос биконов →
    /// (проверенные EndpointId-провайдеры, extra-хэш из v2-ответа, живые сидеры).
    pub(super) async fn discover_extra(
        ih: [u8; 20],
        cache_key: &str,
    ) -> (Vec<String>, Option<String>, u32) {
        // Параллельно: Mainline DHT (интернет) + LAN broadcast (локальная сеть).
        let (peers, lan) = tokio::join!(
            crate::torrent::dht_discover_dapp(ih, 20, 8),
            crate::dht_dapp::lan_probe(ih, std::time::Duration::from_millis(700)),
        );
        let mut ids: Vec<String> = Vec::new();
        let mut xh: Option<String> = None;
        for (e, x) in lan {
            let es = e.to_string();
            if !ids.contains(&es) {
                ids.push(es);
            }
            if xh.is_none() {
                if let Some(xb) = x {
                    xh = Some(Hash::from_bytes(xb).to_string());
                }
            }
        }
        if !peers.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let mut handles = Vec::new();
            for addr in peers.into_iter().take(12) {
                if seen.insert(addr) {
                    handles.push(tokio::spawn(crate::dht_dapp::probe_ex(
                        addr,
                        ih,
                        std::time::Duration::from_secs(2),
                    )));
                }
            }
            for h in handles {
                if let Ok(Some((e, x))) = h.await {
                    let es = e.to_string();
                    if !ids.contains(&es) {
                        ids.push(es);
                    }
                    if xh.is_none() {
                        if let Some(xb) = x {
                            xh = Some(Hash::from_bytes(xb).to_string());
                        }
                    }
                }
            }
        }
        let seeders = ids.len() as u32;
        if !ids.is_empty() {
            let _ = crate::DB.insert(cache_key.as_bytes(), ids.join(",").into_bytes());
        } else if let Ok(Some(v)) = crate::DB.get(cache_key.as_bytes()) {
            // фолбэк на прошлые успешные резолвы (DHT бывает флаки)
            if let Ok(s) = std::str::from_utf8(&v) {
                ids = s
                    .split(',')
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty())
                    .collect();
            }
        }
        (ids, xh, seeders)
    }

    /// Поднять identity-beacon один раз, подписывая тем же iroh-ключом
    /// (`data/node.key`), что и Endpoint — чтобы отдаваемый EndpointId был наш.
    async fn start_dapp_beacon(app: &AppHandle) {
        if DAPP_BEACON_PORT.lock().unwrap().is_some() {
            return;
        }
        let key_path = ensure_data_dir(app).join("node.key");
        let secret = match tokio::fs::read(&key_path).await {
            Ok(b) if b.len() == 32 => {
                let mut a = [0u8; 32];
                a.copy_from_slice(&b);
                iroh::SecretKey::from_bytes(&a)
            }
            _ => return,
        };
        match crate::dht_dapp::spawn_beacon(secret, DAPP_SERVED.clone(), DEV_EXTRA.clone()).await {
            Ok(port) => {
                *DAPP_BEACON_PORT.lock().unwrap() = Some(port);
                crate::syslog::log("dht", format!("dApp identity-beacon on UDP :{}", port));
                // Периодический ре-анонс: записи BEP5 протухают за ~15–30 мин,
                // а разовый анонс при старте мог уйти ДО бутстрапа DHT (и молча
                // потеряться). Повторяем каждые 10 минут для всего served-набора.
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                        let served: Vec<[u8; 20]> =
                            DAPP_SERVED.lock().unwrap().iter().copied().collect();
                        for ih in served {
                            crate::torrent::dht_announce_dapp(ih, port);
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                });
            }
            Err(e) => crate::syslog::log("dht", format!("beacon failed to start: {}", e)),
        }
    }

    /// Трекерлесс: BEP5 `get_peers(app_infohash)` → IP:port’ы → параллельный опрос
    /// identity-beacon’ов → проверенные iroh EndpointId. Кэш в Sled
    /// `dht:providers:<app_id>` (мгновенный старт + устойчивость к флаки DHT).
    async fn dht_discover_providers(app_id: &str) -> Vec<EndpointId> {
        let ih = crate::dht_dapp::app_infohash(app_id);
        let cache_key = format!("dht:providers:{}", app_id);
        let peers = crate::torrent::dht_discover_dapp(ih, 20, 8).await;
        let mut out: Vec<EndpointId> = Vec::new();
        if !peers.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let mut handles = Vec::new();
            for addr in peers.into_iter().take(12) {
                if seen.insert(addr) {
                    handles.push(tokio::spawn(crate::dht_dapp::probe(
                        addr,
                        ih,
                        std::time::Duration::from_secs(2),
                    )));
                }
            }
            for h in handles {
                if let Ok(Some(e)) = h.await {
                    if !out.contains(&e) {
                        out.push(e);
                    }
                }
            }
        }
        if !out.is_empty() {
            let joined = out.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(",");
            let _ = crate::DB.insert(cache_key.as_bytes(), joined.into_bytes());
            crate::syslog::log(
                "dht",
                format!("dApp {}: found {} provider(s) via DHT", app_id, out.len()),
            );
        } else if let Ok(Some(v)) = crate::DB.get(cache_key.as_bytes()) {
            // фолбэк на прошлые успешные резолвы
            if let Ok(s) = std::str::from_utf8(&v) {
                for part in s.split(',') {
                    if let Ok(e) = part.trim().parse::<EndpointId>() {
                        if !out.contains(&e) {
                            out.push(e);
                        }
                    }
                }
            }
        }
        out
    }

    async fn gather_providers(app_id: &str, node_id: &str) -> Vec<EndpointId> {
        let mut ids: Vec<EndpointId> = Vec::new();
        if let Ok(e) = node_id.trim().parse::<EndpointId>() {
            ids.push(e);
        }
        let headless = tokio::task::spawn_blocking(|| {
            super::network_map_blocking()
                .into_iter()
                .filter(|s| !s.metrics_url.trim().is_empty())
                .map(|s| s.endpoint_id)
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        for hid in headless {
            if let Ok(e) = hid.trim().parse::<EndpointId>() {
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
        }
        // Трекерлесс-обнаружение через Mainline DHT + identity-beacon (тумблер).
        if super::dht_providers_enabled_internal() && !app_id.is_empty() {
            for e in dht_discover_providers(app_id).await {
                if !ids.contains(&e) {
                    ids.push(e);
                }
            }
        }
        ids
    }

    pub async fn install(
        app: &AppHandle,
        id: &str,
        node_id: &str,
        hash: &str,
    ) -> Result<(), String> {
        let (_, store, endpoint) = ensure(app).await?;
        let nid: EndpointId = node_id.parse().map_err(|_| "bad node id".to_string())?;
        let h: Hash = hash.parse().map_err(|_| "bad hash".to_string())?;

        // Swarm-ветка (тумблер cfg:swarm_enabled, по умолчанию ON): bandit-failover
        // по всем известным провайдерам — быстрый переход с дохлого пира на живого +
        // память о надёжных сидерах. При любой ошибке — безопасный откат ниже.
        let mut downloaded = false;
        if super::swarm_enabled_internal() {
            let providers = gather_providers(id, node_id).await;
            if !providers.is_empty() {
                let bandit = SWARM_BANDIT.clone();
                match download_blob_bandit(&store, &endpoint, h, &providers, &bandit).await {
                    Ok(()) => {
                        downloaded = true;
                        crate::syslog::log("swarm", format!(
                            "install {}: swarm download succeeded ({} peer(s))",
                            id,
                            providers.len()
                        ));
                    }
                    Err(e) => crate::syslog::log("swarm", format!(
                        "install {}: swarm failed ({}), falling back to direct download",
                        id, e
                    )),
                }
                save_swarm_weights();
            }
        }

        if !downloaded {
        if let Err(primary_err) = download_complete(&store, &endpoint, h, nid).await {
            // WAN fallback (torrent-style): the announced mDNS peer couldn't serve
            // a complete blob (stale hash / partial seed). Try the tracker-known
            // headless seeders — they seed every paid dApp — for the SAME content
            // hash before giving up.
            let headless = tokio::task::spawn_blocking(|| {
                super::network_map_blocking()
                    .into_iter()
                    .filter(|s| !s.metrics_url.trim().is_empty())
                    .map(|s| s.endpoint_id)
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let mut recovered = false;
            for hid in headless {
                let eid = match hid.parse::<EndpointId>() {
                    Ok(e) if e != nid => e,
                    _ => continue,
                };
                if download_complete(&store, &endpoint, h, eid).await.is_ok() {
                    recovered = true;
                    break;
                }
            }
            if !recovered {
                return Err(primary_err);
            }
        }
        }

        let data = ensure_data_dir(app);
        let tmp_tar = data
            .join("blobs")
            .join(format!("dl-{}.tar", super::sanitize(id)));
        let _ = std::fs::create_dir_all(data.join("blobs"));
        store
            .blobs()
            .export(h, &tmp_tar)
            .await
            .map_err(|e| e.to_string())?;

        // Storage quota: reject if unpacking would push data/dapps over the
        // configured limit. The frontend turns the STORAGE_LIMIT error into an
        // "increase storage by N MB" confirm prompt.
        let new_size = std::fs::metadata(&tmp_tar).map(|m| m.len()).unwrap_or(0);
        let dapps = data.join("dapps");
        let projected = super::dir_size(&dapps)
            .saturating_sub(super::dir_size(&dapps.join(id)))
            .saturating_add(new_size);
        let limit = super::get_storage_limit_internal();
        if projected > limit {
            let _ = std::fs::remove_file(&tmp_tar);
            return Err(format!("STORAGE_LIMIT:{}", projected - limit));
        }

        let dest = data.join("dapps").join(id);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let f = std::fs::File::open(&tmp_tar).map_err(|e| e.to_string())?;
        let mut ar = tar::Archive::new(f);
        ar.unpack(&dest).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&tmp_tar);
        Ok(())
    }

    /// Download the tar blob `hash` from `node_id` into the local store WITHOUT
    /// exporting/unpacking it. Used in hybrid (manual) update mode to pre-stage
    /// an update so applying it later (export + swap) is instant.
    pub async fn prefetch(app: &AppHandle, node_id: &str, hash: &str) -> Result<(), String> {
        let (_, store, endpoint) = ensure(app).await?;
        let nid: EndpointId = node_id.parse().map_err(|_| "bad node id".to_string())?;
        let h: Hash = hash.parse().map_err(|_| "bad hash".to_string())?;
        if super::swarm_enabled_internal() {
            let providers = gather_providers("", node_id).await;
            if !providers.is_empty() {
                let bandit = SWARM_BANDIT.clone();
                let r = download_blob_bandit(&store, &endpoint, h, &providers, &bandit).await;
                save_swarm_weights();
                if r.is_ok() {
                    return Ok(());
                }
            }
        }
        download_complete(&store, &endpoint, h, nid).await
    }

    /// Lightweight storage proof: open a blobs connection to `node_id` and OBSERVE
    /// its bitfield for content `hash` (no download). Returns "verified" (peer
    /// holds the full blob), "partial" (holds some), or "unverified" (no proof /
    /// unreachable).
    pub async fn probe_peer_storage(app: &AppHandle, node_id: &str, hash: &str) -> String {
        use n0_future::StreamExt;
        let (_, store, endpoint) = match ensure(app).await {
            Ok(v) => v,
            Err(_) => return "unverified".to_string(),
        };
        let nid: EndpointId = match node_id.parse() {
            Ok(n) => n,
            Err(_) => return "unverified".to_string(),
        };
        let h: Hash = match hash.parse() {
            Ok(h) => h,
            Err(_) => return "unverified".to_string(),
        };
        let conn = match tokio::time::timeout(
            std::time::Duration::from_secs(4),
            endpoint.connect(nid, iroh_blobs::ALPN),
        )
        .await
        {
            Ok(Ok(c)) => c,
            _ => return "unverified".to_string(),
        };
        let req = iroh_blobs::protocol::ObserveRequest::new(h);
        let mut stream = store.remote().observe(conn, req);
        match tokio::time::timeout(std::time::Duration::from_secs(4), stream.next()).await {
            Ok(Some(Ok(bf))) => {
                if bf.is_complete() {
                    "verified".to_string()
                } else if bf.total_bytes() > 0 {
                    "partial".to_string()
                } else {
                    "unverified".to_string()
                }
            }
            _ => "unverified".to_string(),
        }
    }

    // =================================================================
    //  Протокол api:// — клиентская обёртка к netfory-provider
    // =================================================================


    // WS-туннель (v1.55.29): требует ALPN /1
    type WsTxMap =
        tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::mpsc::Sender<WsOut>>>;
    pub(super) static WS_STREAMS: Lazy<WsTxMap> =
        Lazy::new(|| tokio::sync::Mutex::new(std::collections::HashMap::new()));

    #[derive(Clone, Debug)]
    pub(super) enum WsOut {
        Text(String),
        Binary(Vec<u8>),
        Ping(Vec<u8>),
        Pong(Vec<u8>),
        Close { code: u16, reason: String },
    }

    const WS_KIND_TEXT: u8 = 0x01;
    const WS_KIND_BINARY: u8 = 0x02;
    const WS_KIND_PING: u8 = 0x09;
    const WS_KIND_PONG: u8 = 0x0A;
    const WS_KIND_CLOSE: u8 = 0x08;
    const WS_MAX_FRAME: usize = 8 * 1024 * 1024;

    fn ws_encode(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Результат `dapp_ws_open`: id потока + HTTP-статус upstream-хендшейка.
    /// Возвращается прямо из invoke Promise — это ФИКС race condition,
    /// когда событие `dapp-ws-open` эмитилось из Rust РАНЬШЕ, чем JS успевал
    /// назначить streamId (invoke ещё не resolve'ился) → open никогда не
    /// фиксировался, WS-хендшейк на клиенте зависал.
    #[derive(serde::Serialize, Clone, Debug)]
    pub struct WsOpenResult {
        pub stream_id: String,
        pub status: u16,
    }

    pub async fn ws_open(
        app: &tauri::AppHandle,
        url: String,
        headers: std::collections::BTreeMap<String, String>,
    ) -> Result<WsOpenResult, String> {
        use tauri::Emitter;
        let rest = url
            .strip_prefix("ws://")
            .or_else(|| url.strip_prefix("wss://"))
            .ok_or_else(|| "url должен начинаться с ws:// или wss://".to_string())?;
        let (authority, after) = match rest.find('/') {
            Some(i) => (rest[..i].to_string(), &rest[i + 1..]),
            None => (rest.to_string(), ""),
        };
        let (provider, path) = match after.find('/') {
            Some(i) => (after[..i].to_string(), format!("/{}", &after[i + 1..])),
            None => (after.to_string(), "/".to_string()),
        };
        let eid: EndpointId = authority
            .parse()
            .map_err(|_| format!("невалидный NodeID: {authority}"))?;
        if provider.is_empty() {
            return Err("укажите имя провайдера в URL".to_string());
        }
        let (_, _, endpoint) = ensure(app).await?;
        let (conn, v1) = pool_get_or_connect(&endpoint, eid).await?;
        if !v1 {
            return Err(
                "provider does not support WS — upgrade to netfory-provider ≥ 0.6.0".to_string(),
            );
        }
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;

        let hdrs_json: serde_json::Map<String, serde_json::Value> = headers
            .into_iter()
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let request_id = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let packet = serde_json::json!({
            "target_provider": provider,
            "method": "WS",
            "path": path,
            "body": "",
            "headers": hdrs_json,
            "ttl": 3,
            "request_id": request_id,
        });
        let pkt_bytes = serde_json::to_vec(&packet).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(4 + pkt_bytes.len());
        buf.extend_from_slice(&(pkt_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(&pkt_bytes);
        send.write_all(&buf).await.map_err(|e| e.to_string())?;

        let ack = tokio::time::timeout(std::time::Duration::from_secs(10), read_ws_frame(&mut recv))
            .await
            .map_err(|_| "ws handshake timeout".to_string())?
            .map_err(|e| format!("ws handshake read: {e}"))?;
        let status = match ack {
            Some(WsIn::Text(s)) => serde_json::from_str::<serde_json::Value>(&s)
                .ok()
                .and_then(|v| v.get("status").and_then(|x| x.as_u64()))
                .unwrap_or(0) as u16,
            Some(WsIn::Close { code, reason }) => {
                return Err(format!("ws refused by provider: close({code}) {reason}"))
            }
            _ => return Err("unexpected handshake frame".to_string()),
        };

        let stream_id = uuid_v4_hex();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<WsOut>(128);
        {
            let mut m = WS_STREAMS.lock().await;
            m.insert(stream_id.clone(), out_tx);
        }
        let _ = app.emit(
            "dapp-ws-open",
            serde_json::json!({ "streamId": stream_id, "status": status }),
        );

        let sid_w = stream_id.clone();
        let app_w = app.clone();
        tokio::spawn(async move {
            while let Some(cmd) = out_rx.recv().await {
                let bytes = match cmd {
                    WsOut::Text(s) => ws_encode(WS_KIND_TEXT, s.as_bytes()),
                    WsOut::Binary(b) => ws_encode(WS_KIND_BINARY, &b),
                    WsOut::Ping(b) => ws_encode(WS_KIND_PING, &b),
                    WsOut::Pong(b) => ws_encode(WS_KIND_PONG, &b),
                    WsOut::Close { code, reason } => {
                        let mut payload = Vec::with_capacity(2 + reason.len());
                        payload.extend_from_slice(&code.to_be_bytes());
                        payload.extend_from_slice(reason.as_bytes());
                        let _ = send.write_all(&ws_encode(WS_KIND_CLOSE, &payload)).await;
                        let _ = send.finish();
                        break;
                    }
                };
                if send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
            let mut m = WS_STREAMS.lock().await;
            m.remove(&sid_w);
            let _ = app_w.emit(
                "dapp-ws-close",
                serde_json::json!({ "streamId": sid_w, "code": 1006u16, "reason": "writer closed" }),
            );
        });

        let sid_r = stream_id.clone();
        let app_r = app.clone();
        tokio::spawn(async move {
            loop {
                match read_ws_frame(&mut recv).await {
                    Ok(Some(WsIn::Text(s))) => {
                        let _ = app_r.emit(
                            "dapp-ws-message",
                            serde_json::json!({ "streamId": sid_r, "kind": "text", "data": s }),
                        );
                    }
                    Ok(Some(WsIn::Binary(b))) => {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD.encode(&b);
                        let _ = app_r.emit(
                            "dapp-ws-message",
                            serde_json::json!({ "streamId": sid_r, "kind": "binary", "data": b64 }),
                        );
                    }
                    Ok(Some(WsIn::Ping(b))) => {
                        let m = WS_STREAMS.lock().await;
                        if let Some(tx) = m.get(&sid_r) {
                            let _ = tx.try_send(WsOut::Pong(b));
                        }
                    }
                    Ok(Some(WsIn::Pong(_))) => {}
                    Ok(Some(WsIn::Close { code, reason })) => {
                        let mut m = WS_STREAMS.lock().await;
                        m.remove(&sid_r);
                        let _ = app_r.emit(
                            "dapp-ws-close",
                            serde_json::json!({ "streamId": sid_r, "code": code, "reason": reason }),
                        );
                        break;
                    }
                    Ok(None) | Err(_) => {
                        let mut m = WS_STREAMS.lock().await;
                        m.remove(&sid_r);
                        let _ = app_r.emit(
                            "dapp-ws-close",
                            serde_json::json!({ "streamId": sid_r, "code": 1006u16, "reason": "connection lost" }),
                        );
                        break;
                    }
                }
            }
        });

        Ok(WsOpenResult { stream_id, status })
    }

    #[derive(Debug)]
    enum WsIn {
        Text(String),
        Binary(Vec<u8>),
        Ping(Vec<u8>),
        Pong(Vec<u8>),
        Close { code: u16, reason: String },
    }

    async fn read_ws_frame(
        r: &mut iroh::endpoint::RecvStream,
    ) -> Result<Option<WsIn>, String> {
        use tokio::io::AsyncReadExt;
        let mut hdr = [0u8; 5];
        if let Err(e) = r.read_exact(&mut hdr).await {
            // ReadExactError::FinishedEarly когда стрим закрыт до 5 байт → EOF.
            let s = format!("{e}");
            if s.contains("FinishedEarly") || s.contains("finished") {
                return Ok(None);
            }
            return Err(s);
        }
        let kind = hdr[0];
        let len = u32::from_le_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]) as usize;
        if len > WS_MAX_FRAME {
            return Err(format!("ws frame too big: {len}"));
        }
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).await.map_err(|e| e.to_string())?;
        Ok(Some(match kind {
            WS_KIND_TEXT => WsIn::Text(String::from_utf8(payload).map_err(|e| e.to_string())?),
            WS_KIND_BINARY => WsIn::Binary(payload),
            WS_KIND_PING => WsIn::Ping(payload),
            WS_KIND_PONG => WsIn::Pong(payload),
            WS_KIND_CLOSE => {
                let (code, reason) = if payload.len() >= 2 {
                    (
                        u16::from_be_bytes([payload[0], payload[1]]),
                        String::from_utf8_lossy(&payload[2..]).into_owned(),
                    )
                } else {
                    (1005, String::new())
                };
                WsIn::Close { code, reason }
            }
            other => return Err(format!("unknown ws kind: {other:#x}")),
        }))
    }

    pub async fn ws_send(stream_id: String, kind: String, data: String) -> Result<(), String> {
        let m = WS_STREAMS.lock().await;
        let tx = m
            .get(&stream_id)
            .ok_or_else(|| format!("ws stream {stream_id} not found"))?
            .clone();
        drop(m);
        let cmd = match kind.as_str() {
            "text" => WsOut::Text(data),
            "binary" => {
                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data.as_bytes())
                    .map_err(|e| format!("base64 decode: {e}"))?;
                WsOut::Binary(bytes)
            }
            "ping" => WsOut::Ping(data.into_bytes()),
            "pong" => WsOut::Pong(data.into_bytes()),
            other => return Err(format!("unknown ws kind: {other}")),
        };
        tx.send(cmd).await.map_err(|_| "ws channel closed".to_string())?;
        Ok(())
    }

    pub async fn ws_close(stream_id: String, code: u16, reason: String) -> Result<(), String> {
        let mut m = WS_STREAMS.lock().await;
        if let Some(tx) = m.remove(&stream_id) {
            let _ = tx.send(WsOut::Close { code, reason }).await;
        }
        Ok(())
    }

    fn uuid_v4_hex() -> String {
        use rand::RngCore;
        let mut b = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut b);
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    // -----------------------------------------------------------------
    //  api://-запрос (v1.55.24: headers, v1.55.28: pool, v1.55.29: ALPN v1)
    // -----------------------------------------------------------------


    /// Выполнить api://-запрос: подключиться к провайдеру по P2P (по NodeID),
    /// отправить Mesh-пакет с именем целевого эндпоинта, получить JSON-ответ
    /// узла с встроенным `pdata` и проверить подпись провайдера (Ed25519).
    ///
    /// Формат адреса: `api://<nodeId>/<providerName>/<path>`
    ///   * `nodeId` — криптографический адрес узла (нельзя подделать);
    ///   * `providerName` — имя эндпоинта в конфиге узла (`endpoints[].name`),
    ///     селектор «какой local_url проксировать» в контексте этого узла.
    pub async fn api_request_impl(
        app: &AppHandle,
        authority: String,
        provider: String,
        method: String,
        path: String,
        body: String,
        headers: std::collections::BTreeMap<String, String>,
    ) -> Result<super::ApiFetchResult, String> {
        use base64::Engine;

        let (_node_id, _store, endpoint) = ensure(app).await?;

        // authority ДОЛЖЕН быть валидным NodeID (EndpointId). Имя нигде не
        // регистрируется и не резолвится глобально — это защищает от подмены.
        let eid: EndpointId = authority.parse().map_err(|_| {
            format!("невалидный NodeID провайдера: {authority}. Формат: api://<nodeId>/<provider>/<path>")
        })?;
        let target_name = provider;
        let method = if method.is_empty() { "GET".to_string() } else { method.to_uppercase() };
        let body_b64 = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());

        // --- Сборка Mesh-пакета ---
        let request_id = format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        // Прокидываем `headers` в mesh-packet: netfory-провайдер обязан
        // форвардить их в апстрим (например, `Authorization: Bearer <jwt>`
        // от dApp'а). Провайдеры, не знающие про поле, просто игнорят его —
        // JSON forward-compatible.
        let headers_map: serde_json::Map<String, serde_json::Value> = headers
            .into_iter()
            .filter(|(k, _)| {
                // Отсекаем hop-by-hop и потенциально опасные заголовки —
                // Host/Connection/Content-Length провайдер должен ставить сам,
                // Origin/Referer от WebView мы тоже не таскаем (запрос идёт
                // не из браузерного контекста апстрима).
                let l = k.to_ascii_lowercase();
                !matches!(
                    l.as_str(),
                    "host"
                        | "connection"
                        | "content-length"
                        | "transfer-encoding"
                        | "upgrade"
                        | "proxy-connection"
                        | "keep-alive"
                        | "origin"
                        | "referer"
                        | "user-agent"
                )
            })
            .map(|(k, v)| (k, serde_json::Value::String(v)))
            .collect();
        let packet = serde_json::json!({
            "target_provider": target_name,
            "method": method,
            "path": path,
            "body": body_b64,      // base64 тела запроса
            "headers": headers_map, // whitelisted HTTP-заголовки от dApp
            "ttl": 5,
            "request_id": request_id,
        });
        let packet_bytes = serde_json::to_vec(&packet).map_err(|e| e.to_string())?;

        // --- ConnectionPool с ALPN /1 предпочтением + /0 fallback ---
        let (conn, v1) = match pool_get_or_connect(&endpoint, eid).await {
            Ok(c) => c,
            Err(e) => return Err(e),
        };

        let (mut send, mut recv) = match conn.open_bi().await {
            Ok(sr) => sr,
            Err(_) => {
                pool_drop(&eid).await;
                let (conn, _v1) = pool_get_or_connect(&endpoint, eid).await?;
                conn.open_bi().await.map_err(|e| e.to_string())?
            }
        };

        // Handshake: v1 — [LE_u32 len][JSON]; v0 — сырой JSON + finish() (EOF).
        if v1 {
            let mut buf = Vec::with_capacity(4 + packet_bytes.len());
            buf.extend_from_slice(&(packet_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&packet_bytes);
            send.write_all(&buf).await.map_err(|e| e.to_string())?;
        } else {
            send.write_all(&packet_bytes).await.map_err(|e| e.to_string())?;
        }
        send.finish().map_err(|e| e.to_string())?;

        let raw = tokio::time::timeout(
            std::time::Duration::from_secs(14),
            recv.read_to_end(8 * 1024 * 1024),
        )
        .await
        .map_err(|_| "таймаут ответа провайдера".to_string())?
        .map_err(|e| e.to_string())?;

        // ВАЖНО: connection остаётся в pool для reuse.

        // --- Разбор ответа и проверка подписи ---
        let value: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|e| format!("ответ не JSON: {e}"))?;
        let pdata = value
            .get("pdata")
            .ok_or_else(|| "в ответе нет объекта pdata".to_string())?;

        let prov_node = pdata.get("node_id").and_then(|v| v.as_str()).unwrap_or("");
        let status = pdata.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        let signed_at = pdata.get("signed_at").and_then(|v| v.as_u64()).unwrap_or(0);
        let sig_hex = pdata.get("sig").and_then(|v| v.as_str()).unwrap_or("");
        let body_b64 = pdata.get("body_b64").and_then(|v| v.as_str()).unwrap_or("");
        let relayed = pdata.get("relayed").and_then(|v| v.as_bool()).unwrap_or(false);

        // Канонические байты: node_id|status|signed_at| + исходное тело узла.
        let verified = (|| -> Option<bool> {
            let body = base64::engine::general_purpose::STANDARD
                .decode(body_b64.as_bytes())
                .ok()?;
            let mut canon = format!("{prov_node}|{status}|{signed_at}|").into_bytes();
            canon.extend_from_slice(&body);

            let pk: EndpointId = prov_node.parse().ok()?;
            let sig_bytes = hex::decode(sig_hex).ok()?;
            if sig_bytes.len() != 64 {
                return None;
            }
            let mut sarr = [0u8; 64];
            sarr.copy_from_slice(&sig_bytes);
            let sig = iroh::Signature::from_bytes(&sarr);
            Some(pk.verify(&canon, &sig).is_ok())
        })()
        .unwrap_or(false);

        // Усиление: для ПРЯМОГО запроса (без реле) подписавший узел обязан
        // совпадать с запрошенным NodeID — иначе первый хоп мог подсунуть
        // валидно подписанный, но чужой ответ.
        let verified = verified && (relayed || prov_node == authority);

        // Красиво форматируем весь ответ (data + pdata) для вывода.
        let body_pretty =
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| String::from_utf8_lossy(&raw).to_string());

        Ok(super::ApiFetchResult {
            body: body_pretty,
            verified,
            status,
            provider_node_id: prov_node.to_string(),
            relayed,
            target: target_name,
            signed_at,
        })
    }
}

#[tauri::command]
pub(crate) async fn p2p_status(app: tauri::AppHandle) -> P2pStatus {
    P2pStatus {
        enabled: p2p_impl::enabled(),
        node_id: p2p_impl::node_id(&app).await.unwrap_or_default(),
    }
}

/// Результат api://-запроса для фронтенда.
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiFetchResult {
    /// Полное JSON-тело ответа узла (data + pdata), красиво отформатированное.
    pub body: String,
    /// Прошла ли проверка подписи провайдера (Ed25519 по NodeID).
    pub verified: bool,
    /// HTTP-статус апстрима (из pdata).
    pub status: u16,
    /// NodeID провайдера-исполнителя.
    pub provider_node_id: String,
    /// Был ли ответ получен через ретрансляцию.
    pub relayed: bool,
    /// Имя/адрес цели (для отображения).
    pub target: String,
    /// Unix-timestamp подписи ответа (для sanity-checks клиента).
    #[serde(default)]
    pub signed_at: u64,
}

/// Разобрать `api://<nodeId>/<provider>/<path>` и выполнить P2P-запрос с
/// указанным HTTP-методом, телом и опциональными HTTP-заголовками (например
/// `Authorization: Bearer <jwt>` от dApp'а). Используется как фронтендом
/// (`api_fetch`), так и внутренним слоем доступа к узлу (`sth_node`).
/// Наш EndpointId (hex) — если endpoint уже поднят.
#[cfg(feature = "p2p")]
pub(crate) fn my_endpoint_id_hex() -> Option<String> {
    p2p_impl::endpoint_now().map(|ep| ep.id().to_string())
}

/// iroh Endpoint главного приложения для AI-клиента (`ai_client.rs`).
#[cfg(feature = "p2p")]
pub(crate) async fn ai_iroh_endpoint(app: &tauri::AppHandle) -> Result<iroh::Endpoint, String> {
    p2p_impl::ai_endpoint(app).await
}

/// connect с адресами из sled-книги пиров и fallback на discovery (см. `peerbook.rs`).
#[cfg(feature = "p2p")]
pub(crate) async fn connect_known(
    endpoint: &iroh::Endpoint,
    node_id: iroh::EndpointId,
    alpn: &'static [u8],
    deadline: std::time::Duration,
) -> Result<iroh::endpoint::Connection, String> {
    p2p_impl::connect_known(endpoint, node_id, alpn, deadline).await
}

#[cfg(feature = "p2p")]
pub(crate) async fn api_request(
    app: tauri::AppHandle,
    url: String,
    method: String,
    body: String,
    headers: std::collections::BTreeMap<String, String>,
) -> Result<ApiFetchResult, String> {
    let rest = url
        .strip_prefix("api://")
        .ok_or_else(|| "адрес должен начинаться с api://".to_string())?;

    // 1) authority (nodeId) — до первого '/'.
    let (authority, after) = match rest.find('/') {
        Some(i) => (rest[..i].to_string(), &rest[i + 1..]),
        None => (rest.to_string(), ""),
    };
    // 2) provider (имя эндпоинта) — первый сегмент остатка; дальше — path.
    let (provider, path) = match after.find('/') {
        Some(i) => (after[..i].to_string(), format!("/{}", &after[i + 1..])),
        None => (after.to_string(), "/".to_string()),
    };

    if authority.is_empty() {
        return Err("пустой NodeID провайдера".to_string());
    }
    if provider.is_empty() {
        return Err("укажите имя провайдера: api://<nodeId>/<provider>/<path>".to_string());
    }
    p2p_impl::api_request_impl(&app, authority, provider, method, path, body, headers).await
}

/// Разобрать `api://<nodeId>/<provider>/<path>` и выполнить P2P-запрос (GET).
#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn api_fetch(app: tauri::AppHandle, url: String) -> Result<ApiFetchResult, String> {
    api_request(app, url, "GET".to_string(), String::new(), Default::default()).await
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn api_fetch(_app: tauri::AppHandle, _url: String) -> Result<ApiFetchResult, String> {
    Err("Протокол api:// доступен только в десктоп-сборке (P2P).".to_string())
}

// --- WS-туннель для dApp'ов (v1.55.29, требует netfory-provider ≥ 0.6.0) ---

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn dapp_ws_open(
    app: tauri::AppHandle,
    url: String,
    headers: std::collections::BTreeMap<String, String>,
) -> Result<p2p_impl::WsOpenResult, String> {
    p2p_impl::ws_open(&app, url, headers).await
}
#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn dapp_ws_open(
    _app: tauri::AppHandle,
    _url: String,
    _headers: std::collections::BTreeMap<String, String>,
) -> Result<serde_json::Value, String> {
    Err("WS доступен только в десктоп-сборке (P2P)".into())
}

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn dapp_ws_send(stream_id: String, kind: String, data: String) -> Result<(), String> {
    p2p_impl::ws_send(stream_id, kind, data).await
}
#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn dapp_ws_send(_stream_id: String, _kind: String, _data: String) -> Result<(), String> {
    Err("WS доступен только в десктоп-сборке (P2P)".into())
}

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn dapp_ws_close(
    stream_id: String,
    code: Option<u16>,
    reason: Option<String>,
) -> Result<(), String> {
    p2p_impl::ws_close(stream_id, code.unwrap_or(1000), reason.unwrap_or_default()).await
}
#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn dapp_ws_close(
    _stream_id: String,
    _code: Option<u16>,
    _reason: Option<String>,
) -> Result<(), String> {
    Ok(())
}

/// Fetch a peer's dApp over iroh-blobs, register it locally and return the
/// manifest (read back from the freshly-exported content.json).
#[tauri::command]
pub(crate) async fn install_app(
    app: tauri::AppHandle,
    id: String,
    node_id: String,
    hash: String,
) -> Result<Manifest, String> {
    p2p_impl::install(&app, &id, &node_id, &hash).await?;
    let content = ensure_data_dir(&app).join("dapps").join(&id).join("content.json");
    let raw = std::fs::read(&content).map_err(|e| e.to_string())?;
    let mut manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
    DB.insert(format!("{}{}", APP_PREFIX, id).as_bytes(), raw)
        .map_err(|e| e.to_string())?;
    DB.insert(format!("announce:{}", id).as_bytes(), now_iso().into_bytes())
        .map_err(|e| e.to_string())?;
    // Источник установки для бейджа вкладки (⇄ P2P)
    let src = serde_json::json!({
        "kind": "p2p",
        "node": node_id,
        "at": now_iso(),
        "version": manifest.version,
    });
    let _ = DB.insert(format!("install_src:{}", id).as_bytes(), src.to_string().into_bytes());
    let _ = DB.flush();
    // Приватный dApp: кэшируем access.json в Sled (анонсы/декор UI) и
    // подставляем расшифрованные метаданные для интерфейса.
    if manifest.private == Some(true) {
        crate::private_dapp::cache_access_from_folder(&app, &id);
        crate::private_dapp::decorate_manifest(&mut manifest);
    }
    Ok(manifest)
}

/// Pre-stage a published update over iroh-blobs (downloads the new blob into the
/// local store but does NOT apply it). Used by hybrid (manual) update mode so a
/// later `install_app` apply is fast. No-op error is tolerated by the caller.
#[tauri::command]
pub(crate) async fn prefetch_app(
    app: tauri::AppHandle,
    node_id: String,
    hash: String,
) -> Result<(), String> {
    p2p_impl::prefetch(&app, &node_id, &hash).await
}

/// PNG-логотип НЕустановленного dApp из каталога. Content-addressed кэш
/// `data/discovery_cache/<appId>/logo-<hash>.png`; при промахе — ленивая
/// загрузка iroh-блоба у переданных P2P-провайдеров (только PNG, ≤ 256 KB).
#[tauri::command]
pub(crate) async fn discovery_logo(
    app: tauri::AppHandle,
    id: String,
    logo_hash: String,
    node_ids: Vec<String>,
) -> Option<String> {
    discovery_media(app, id, logo_hash, node_ids, "logo", false).await
}

/// DAPP Preview 440×180 (PNG/JPG) НЕустановленного dApp — та же механика:
/// кэш `data/discovery_cache/<appId>/preview-<hash>.<ext>` + ленивый P2P-fetch.
#[tauri::command]
pub(crate) async fn discovery_preview(
    app: tauri::AppHandle,
    id: String,
    preview_hash: String,
    node_ids: Vec<String>,
) -> Option<String> {
    discovery_media(app, id, preview_hash, node_ids, "preview", true).await
}

/// Общий конвейер каталожных медиа (logo/preview): content-addressed кэш
/// `data/discovery_cache/<appId>/<prefix>-<hash>.<png|jpg>`; при промахе —
/// загрузка iroh-блоба (лимит 256 KB; PNG всегда, JPEG — только для preview).
async fn discovery_media(
    app: tauri::AppHandle,
    id: String,
    hash: String,
    node_ids: Vec<String>,
    prefix: &str,
    allow_jpeg: bool,
) -> Option<String> {
    // hash — только ASCII-буквоцифры (iroh Hash): защита от path traversal
    let safe_id = sanitize(&id);
    let short: String = id.chars().take(12).collect();
    if safe_id.is_empty() {
        return None;
    }
    let cache_root = ensure_data_dir(&app).join("discovery_cache");
    let dir = cache_root.join(&safe_id);
    if hash.is_empty() {
        // Анонс без хэша (транзиентно после правки метаданных у паблишера,
        // или источник без медиа-полей) — НЕ повод сбрасывать картинку:
        // отдаём последний закэшированный файл, в сеть не ходим.
        return cached_media_any(&dir, prefix, allow_jpeg);
    }
    if hash.len() > 64 || !hash.chars().all(|c| c.is_ascii_alphanumeric()) {
        crate::syslog::log(
            "dapp",
            format!("discovery_{} {}… — невалидный hash в анонсе", prefix, short),
        );
        return None;
    }
    // кэш-попадание: расширение зависит от контента — проверяем оба
    for ext in ["png", "jpg"] {
        if ext == "jpg" && !allow_jpeg {
            continue;
        }
        if let Ok(b) = std::fs::read(dir.join(format!("{}-{}.{}", prefix, hash, ext))) {
            if !b.is_empty() {
                return Some(media_data_url(&b, ext));
            }
        }
    }
    let tmp = dir.join(format!("{}-{}.tmp", prefix, hash));
    if let Err(e) = p2p_impl::fetch_logo(&app, &hash, &node_ids, &tmp).await {
        // P2P-провайдеры офлайн — облачный fallback: сидеры реплицируют те же
        // медиа в /ntfryseed/discovery_cache (целостность — blake3 = хэш анонса)
        match crate::cloudseeder::fetch_discovery_media(&safe_id, prefix, &hash, allow_jpeg).await
        {
            Some(b) => {
                let _ = std::fs::create_dir_all(&dir);
                if std::fs::write(&tmp, &b).is_err() {
                    return None;
                }
                crate::syslog::log(
                    "dapp",
                    format!(
                        "discovery_{} {}… — P2P fail ({} провайдеров: {}), взят с cloud seeder ({} B)",
                        prefix,
                        short,
                        node_ids.len(),
                        e,
                        b.len()
                    ),
                );
            }
            None => {
                // Кэш сбрасываем только «в крайнем случае» — когда реально
                // скачан НОВЫЙ файл. Пока сеть молчит, отдаём устаревший кэш.
                let stale = cached_media_any(&dir, prefix, allow_jpeg);
                crate::syslog::log(
                    "dapp",
                    format!(
                        "discovery_{} {}… — fetch fail ({} провайдеров): {}; cloud fallback тоже пуст{}",
                        prefix,
                        short,
                        node_ids.len(),
                        e,
                        if stale.is_some() { " — отдан устаревший кэш" } else { "" }
                    ),
                );
                return stale;
            }
        }
    }
    let b = match std::fs::read(&tmp) {
        Ok(b) => b,
        Err(e) => {
            crate::syslog::log(
                "dapp",
                format!("discovery_{} {}… — блоб скачан, но не читается: {}", prefix, short, e),
            );
            return None;
        }
    };
    // недобросовестный пир: неверный формат или больше лимита — отбраковка
    let ext = if b.starts_with(&PNG_MAGIC) {
        "png"
    } else if allow_jpeg && b.starts_with(&JPEG_MAGIC) && !jpeg_is_arithmetic(&b) {
        "jpg"
    } else {
        crate::syslog::log(
            "dapp",
            format!("discovery_{} {}… — блоб не PNG/JPG (или арифм. JPEG), отбраковка", prefix, short),
        );
        let _ = std::fs::remove_file(&tmp);
        return None;
    };
    if b.len() > LOGO_MAX_BYTES {
        crate::syslog::log(
            "dapp",
            format!("discovery_{} {}… — блоб больше лимита ({} B), отбраковка", prefix, short, b.len()),
        );
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    // смена медиа у приложения (новый hash) — чистим старые файлы этого типа
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let cur = format!("{}-{}.tmp", prefix, hash);
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(&format!("{}-", prefix)) && n != cur {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let _ = std::fs::rename(&tmp, dir.join(format!("{}-{}.{}", prefix, hash, ext)));
    prune_discovery_cache(&cache_root);
    crate::syslog::log(
        "dapp",
        format!("discovery_{} {}… — OK: скачан и закэширован ({} B, {})", prefix, short, b.len(), ext),
    );
    Some(media_data_url(&b, ext))
}

/// data-URL по расширению медиа-файла каталога (png/jpg).
fn media_data_url(bytes: &[u8], ext: &str) -> String {
    let mime = if ext == "jpg" { "image/jpeg" } else { "image/png" };
    format!("data:{};base64,{}", mime, BASE64_STANDARD.encode(bytes))
}

/// Последний закэшированный медиа-файл приложения с данным префиксом
/// (logo/preview) НЕЗАВИСИМО от хэша — устойчивость к анонсам без хэша
/// и к недоступности новой версии файла.
fn cached_media_any(dir: &std::path::Path, prefix: &str, allow_jpeg: bool) -> Option<String> {
    let rd = std::fs::read_dir(dir).ok()?;
    let want = format!("{}-", prefix);
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !n.starts_with(&want) {
            continue;
        }
        let ext = if n.ends_with(".png") {
            "png"
        } else if allow_jpeg && n.ends_with(".jpg") {
            "jpg"
        } else {
            continue;
        };
        if let Ok(b) = std::fs::read(e.path()) {
            if !b.is_empty() {
                return Some(media_data_url(&b, ext));
            }
        }
    }
    None
}

/// Прунинг discovery_cache: больше 300 папок приложений → удаляем самые
/// старые по mtime (LRU по времени последней записи).
fn prune_discovery_cache(root: &std::path::Path) {
    const MAX_DIRS: usize = 300;
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let t = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            (t, e.path())
        })
        .collect();
    if dirs.len() <= MAX_DIRS {
        return;
    }
    dirs.sort_by_key(|(t, _)| *t);
    let excess = dirs.len() - MAX_DIRS;
    for (_, p) in dirs.into_iter().take(excess) {
        let _ = std::fs::remove_dir_all(&p);
    }
}

/// Size (bytes) of the P2P blob store on disk (data/blobs).
#[tauri::command(async)]
pub(crate) fn blob_store_usage(app: tauri::AppHandle) -> u64 {
    p2p_impl::blob_store_bytes(&app)
}

/// Reclaim disk by deleting blob-store side files that are no longer needed:
/// loose seed tars whose dApp folder is gone, and leftover `dl-*.tar` downloads
/// (created mid-install, normally removed but stranded on a crash). Pure FS, so
/// it works regardless of the `p2p` feature. Returns the bytes freed.
pub(crate) fn clean_orphaned_blob_files(app: &tauri::AppHandle) -> u64 {
    let data = ensure_data_dir(app);
    let blobs = data.join("blobs");
    let dapps = data.join("dapps");
    let mut freed = 0u64;

    if let Ok(rd) = std::fs::read_dir(&blobs) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("dl-") && name.ends_with(".tar") {
                freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    let seed_dir = blobs.join("seed");
    if let Ok(rd) = std::fs::read_dir(&seed_dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().to_string();
            if let Some(id) = fname.strip_suffix(".tar") {
                if !dapps.join(id).is_dir() {
                    freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    freed
}

/// Physically delete the entire iroh blob store + node identity. Used by the
/// "Wipe all" danger action so NO orphaned blobs survive a full reset (the old
/// wipe only cleared dapps/manifests/keys, leaving data/blobs untouched).
pub(crate) fn wipe_blob_store_dir(app: &tauri::AppHandle) {
    let data = ensure_data_dir(app);
    let _ = std::fs::remove_dir_all(data.join("blobs"));
    let _ = std::fs::remove_file(data.join("node.key"));
    let _ = std::fs::create_dir_all(data.join("blobs"));
}

/// "Clear excess": immediately reclaim disk, then refresh the GC keep-set.
/// Step 1 (immediate, reliable, cross-platform): delete orphaned loose tars
/// (deleted-app seed archives + stranded downloads) — these are plain files the
/// iroh store never garbage-collects. Step 2: re-seed so the in-store iroh GC
/// (120 s background sweep) reclaims OLD blob versions using a fresh keep-set.
#[tauri::command]
pub(crate) async fn optimize_blob_store(app: tauri::AppHandle) -> Result<u64, String> {
    let _ = clean_orphaned_blob_files(&app);
    let _ = p2p_impl::seed_all(&app).await?;
    Ok(p2p_impl::blob_store_bytes(&app))
}

/// Seed a single File Exchange file over iroh (best-effort). Returns the content
/// hash on success, or None when the P2P feature/node is unavailable.
pub(crate) async fn seed_file(app: &tauri::AppHandle, path: &str) -> Option<String> {
    p2p_impl::seed_file(app, path)
        .await
        .ok()
        .filter(|s| !s.is_empty())
}

/// Download a File Exchange blob from a seeder and export raw bytes to `dest`.
pub(crate) async fn fetch_file(
    app: &tauri::AppHandle,
    node_id: &str,
    hash: &str,
    dest: &str,
    extra_providers: &[String],
) -> Result<String, String> {
    p2p_impl::fetch_file(app, node_id, hash, dest, extra_providers).await
}

/// EndpointId выделенного медиа-канала мессенджера (None без p2p/до старта).
pub(crate) async fn media_node_id(app: &tauri::AppHandle) -> Option<String> {
    p2p_impl::media_node_id(app).await.ok().filter(|s| !s.is_empty())
}

/// Скачать медиа мессенджера через выделенный endpoint (изоляция от gossip).
pub(crate) async fn fetch_media_file(
    app: &tauri::AppHandle,
    node_id: &str,
    hash: &str,
    dest: &str,
    extra_providers: &[String],
) -> Result<String, String> {
    p2p_impl::fetch_media(app, node_id, hash, dest, extra_providers).await
}

/// Прямая доставка чат-конверта на узел (минуя gossip).
pub(crate) async fn chat_direct_send(
    app: &tauri::AppHandle,
    node: &str,
    room_id: &str,
    cipher: &str,
) -> bool {
    p2p_impl::chat_direct_send(app, node, room_id, cipher).await
}

/// Live download progress for a blob: (bytes transferred so far, complete?).
pub(crate) async fn blob_progress(app: &tauri::AppHandle, hash: &str) -> (u64, bool) {
    p2p_impl::blob_progress(app, hash).await
}

/// DevHub: немедленно анонсировать автора (его dev-infohash) в DHT + бикон.
pub(crate) fn announce_dev_user(address: &str) {
    p2p_impl::announce_dev(address);
}

/// DevHub: зарегистрировать хэш blob'а своего индекса для v2-ответа бикона.
pub(crate) fn register_dev_index_hash(address: &str, hash_hex: &str) {
    p2p_impl::register_dev_index_hash(address, hash_hex);
}

/// DevHub: blake3-хэш blob'а своего индекса (для gossip-анонса).
pub(crate) fn dev_index_hash_for(address: &str) -> Option<String> {
    p2p_impl::dev_index_hash(address)
}

/// DevHub: найти провайдеров автора → (EndpointId'ы, хэш индекса, живые сидеры).
pub(crate) async fn devhub_discover(address: &str) -> (Vec<String>, Option<String>, u32) {
    p2p_impl::dev_discover(address).await
}

/// E2EE-альбомы: анонс keyfile'а (ECIES-обёрнутые ключи покупателей) в DHT.
#[cfg(feature = "p2p")]
pub(crate) fn announce_album_keys(album_id: &str, hash_hex: &str) {
    p2p_impl::announce_extra(crate::dht_dapp::album_infohash(album_id), hash_hex);
}
#[cfg(not(feature = "p2p"))]
pub(crate) fn announce_album_keys(_album_id: &str, _hash_hex: &str) {}

/// E2EE-альбомы: найти владельца и хэш актуального keyfile'а.
#[cfg(feature = "p2p")]
pub(crate) async fn album_keys_discover(album_id: &str) -> (Vec<String>, Option<String>) {
    let ih = crate::dht_dapp::album_infohash(album_id);
    let cache_key = format!("dht:providers:albkeys:{}", album_id);
    let (ids, mut xh, _) = p2p_impl::discover_extra(ih, &cache_key).await;
    // Фолбэк: хэш keyfile'а из gossip-анонса владельца (бикон за NAT молчит).
    if xh.is_none() {
        xh = crate::DB
            .get(format!("social:albgossip:xh:{}", album_id).as_bytes())
            .ok()
            .flatten()
            .and_then(|v| String::from_utf8(v.to_vec()).ok())
            .filter(|s| !s.is_empty());
    }
    (ids, xh)
}
#[cfg(not(feature = "p2p"))]
pub(crate) async fn album_keys_discover(_album_id: &str) -> (Vec<String>, Option<String>) {
    (Vec::new(), None)
}

/// Immediately reclaim a just-deleted app's disk footprint. Called right after
/// `delete_app` purges the dApp folder + loose tars. Two effects:
///   1. `clean_orphaned_blob_files` — drop any stray seed/download tars (pure FS).
///   2. Re-seed from the REMAINING apps so `SEED_HASHES` (the GC keep-set) no
///      longer protects the deleted app's content blob → the next 120 s GC pass
///      physically reclaims it from the iroh store.
///
/// DEDUP-SAFE: the keep-set is rebuilt from what we still seed, so a blob shared
/// by another local app stays protected — shared content (and the network's
/// ability to serve it) is never harmed. NO-OP when the P2P node isn't running:
/// nothing is loaded to GC, and we never start the endpoint just to reclaim.
pub(crate) async fn reclaim_after_delete(app: &tauri::AppHandle) {
    let _ = clean_orphaned_blob_files(app);
    if !p2p_impl::is_active() {
        return;
    }
    if let Ok(seeded) = p2p_impl::seed_all(app).await {
        let map: HashMap<String, String> = seeded.into_iter().collect();
        rebuild_dedup_index(&map);
    }
}

/// Debounced fire-and-forget variant used by `delete_app`. When the user
/// bulk-deletes several installed dApps in a row every individual reclaim was
/// re-running `seed_all()` — 3–4 seconds of hidden work between each click.
/// This coalesces all deletes that happen within 500 ms into a single reclaim
/// pass at the end of the burst.
///
/// Implementation: a monotonic tick counter. Every call bumps the tick and
/// spawns a task that waits 500 ms then checks — if the observed tick still
/// matches, this is the LAST call of the burst and reclaim runs; otherwise
/// bail out and let the next scheduled task do the work.
static RECLAIM_TICK: once_cell::sync::Lazy<std::sync::atomic::AtomicU64> =
    once_cell::sync::Lazy::new(|| std::sync::atomic::AtomicU64::new(0));

pub(crate) fn schedule_reclaim_after_delete(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    let tick = RECLAIM_TICK.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // Not the last click of the burst? Bail — the next scheduled task
        // will observe its own matching tick and run the reclaim then.
        if RECLAIM_TICK.load(Ordering::SeqCst) != tick {
            return;
        }
        reclaim_after_delete(&app).await;
    });
}


// ============================================================
//  Cross-app content dedup registry (`iroh-blobs` is content-addressed)
//
//  The iroh FsStore already stores byte-identical content under ONE hash, so
//  two dApps shipping the same files share a single physical blob automatically.
//  This registry makes that visible + actionable: `dedup:index` maps each
//  content blob hash -> the set of appIds backed by it. From it we report how
//  many app copies are deduplicated and (with `download_complete`'s already-
//  present short-circuit) we never re-transfer a blob another app already holds.
// ============================================================

const DEDUP_INDEX_KEY: &[u8] = b"dedup:index";

/// Rebuild the hash -> [appIds] index from the CURRENT locally-seeded apps.
/// Authoritative + self-healing: stale hashes (old versions / deleted apps)
/// simply drop out on the next seed cycle.
pub(crate) fn rebuild_dedup_index(seeded: &HashMap<String, String>) {
    let mut idx: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (id, hash) in seeded {
        if hash.is_empty() {
            continue;
        }
        idx.entry(hash.clone()).or_default().push(id.clone());
    }
    for v in idx.values_mut() {
        v.sort();
        v.dedup();
    }
    if let Ok(j) = serde_json::to_vec(&idx) {
        let _ = DB.insert(DEDUP_INDEX_KEY, j);
        let _ = DB.flush();
    }
}

fn load_dedup_index() -> std::collections::BTreeMap<String, Vec<String>> {
    DB.get(DEDUP_INDEX_KEY)
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_slice(&v).ok())
        .unwrap_or_default()
}

#[derive(serde::Serialize)]
pub(crate) struct DedupGroup {
    pub hash: String,
    pub app_ids: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct DedupStats {
    /// distinct content blobs (== physical blobs stored once each)
    pub unique_blobs: usize,
    /// total app->blob references tracked
    pub tracked_apps: usize,
    /// blobs shared by more than one app
    pub shared_blobs: usize,
    /// app copies avoided by content addressing (tracked_apps - unique_blobs)
    pub deduped_apps: usize,
    /// disk bytes saved by content addressing (Σ shared-blob size × (copies-1))
    pub saved_bytes: u64,
    /// only the shared groups, for UI display
    pub groups: Vec<DedupGroup>,
}

/// Stats for the cross-app dedup registry: how many app copies collapse onto a
/// shared content blob, and the disk bytes that saves. Pure sled + FS read —
/// available regardless of the `p2p` feature (index refreshed each seed cycle).
#[tauri::command(async)]
pub(crate) fn dedup_stats(app: tauri::AppHandle) -> DedupStats {
    let idx = load_dedup_index();
    let unique_blobs = idx.len();
    let tracked_apps: usize = idx.values().map(|v| v.len()).sum();
    let seed_dir = ensure_data_dir(&app).join("blobs").join("seed");
    let mut saved_bytes = 0u64;
    let mut groups: Vec<DedupGroup> = idx
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(hash, app_ids)| {
            // Apps in a group share byte-identical content → one physical copy.
            // Saved = (copies - 1) × the size of a representative seed tar.
            let one = app_ids
                .iter()
                .find_map(|id| std::fs::metadata(seed_dir.join(format!("{}.tar", id))).ok())
                .map(|m| m.len())
                .unwrap_or(0);
            saved_bytes = saved_bytes.saturating_add(one.saturating_mul((app_ids.len() as u64).saturating_sub(1)));
            DedupGroup { hash, app_ids }
        })
        .collect();
    groups.sort_by(|a, b| b.app_ids.len().cmp(&a.app_ids.len()));
    DedupStats {
        unique_blobs,
        tracked_apps,
        shared_blobs: groups.len(),
        deduped_apps: tracked_apps.saturating_sub(unique_blobs),
        saved_bytes,
        groups,
    }
}


// ============================================================
//  Storage proof — colour connected peers by whether they verifiably hold
//  content (iroh-blobs `observe`/bitfield as a lightweight proof).
//    verified  → green  (holds the full content blob)
//    partial   → orange (holds some chunks)
//    unverified→ blue   (no proof / unreachable / feature off)
// ============================================================

#[derive(serde::Serialize)]
pub(crate) struct PeerProof {
    pub id: String,
    pub status: String,
}

/// A representative content hash to probe peers against: any blob we know
/// locally (headless seeders hold every paid dApp, so one is enough).
#[cfg(feature = "p2p")]
fn representative_hash() -> String {
    load_dedup_index().keys().next().cloned().unwrap_or_default()
}

/// Probe connected peers for a lightweight storage proof. `peer_ids` are iroh
/// endpoint ids (the markers shown on the 3D globe). Runs concurrently with a
/// short per-peer timeout. Returns each peer's proof status.
#[tauri::command]
pub(crate) async fn verify_peers_storage(
    app: tauri::AppHandle,
    peer_ids: Vec<String>,
) -> Vec<PeerProof> {
    // Bound the fan-out so a huge peer list can't open hundreds of connections.
    let ids: Vec<String> = peer_ids.into_iter().take(48).collect();

    #[cfg(not(feature = "p2p"))]
    {
        let _ = app;
        return ids
            .into_iter()
            .map(|id| PeerProof { id, status: "unverified".to_string() })
            .collect();
    }

    #[cfg(feature = "p2p")]
    {
        let hash = tokio::task::spawn_blocking(representative_hash)
            .await
            .unwrap_or_default();
        if hash.is_empty() {
            return ids
                .into_iter()
                .map(|id| PeerProof { id, status: "unverified".to_string() })
                .collect();
        }
        let mut handles = Vec::with_capacity(ids.len());
        for id in ids {
            let app2 = app.clone();
            let hash2 = hash.clone();
            handles.push(tokio::spawn(async move {
                let status = p2p_impl::probe_peer_storage(&app2, &id, &hash2).await;
                PeerProof { id, status }
            }));
        }
        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            if let Ok(p) = h.await {
                out.push(p);
            }
        }
        out
    }
}

// ============================================================
//  «Сетевой доктор» (v1.63.13) — диагностика мобильных сетей.
//  Пробы: IPv6, dual-stack бинд, UDP/QUIC (QAD), NAT-тип, реле
//  (QUIC + TCP/TLS 443), Mainline DHT, DNS/DoH → вердикт.
// ============================================================

#[derive(serde::Serialize, Clone)]
pub(crate) struct DoctorCheck {
    pub id: String,
    /// ok | warn | fail
    pub status: String,
    pub detail: String,
}

/// Добавить шаг в отчёт: событие для UI + строка в SystemConsole (scope=net).
#[cfg(feature = "p2p")]
fn doctor_push(
    app: &tauri::AppHandle,
    out: &mut Vec<DoctorCheck>,
    id: &str,
    status: &str,
    detail: String,
) {
    use tauri::Emitter;
    let c = DoctorCheck { id: id.to_string(), status: status.to_string(), detail };
    crate::syslog::log("net", format!("doctor {}: [{}] {}", c.id, c.status, c.detail));
    let _ = app.emit("net-doctor-step", &c);
    out.push(c);
}

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn net_doctor_run(app: tauri::AppHandle) -> Vec<DoctorCheck> {
    p2p_impl::doctor_run(&app).await
}

/// Один источник адресов узла в отчёте discovery (без IP — только счётчики).
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct DiscoveryHit {
    pub source: String,
    pub ms: u64,
    pub relays: usize,
    pub addrs: usize,
    pub relay: String,
}

/// Отчёт `discovery_probe`: источники, время резолва, тестовый connect.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct DiscoveryReport {
    pub id: String,
    pub hits: Vec<DiscoveryHit>,
    pub errors: Vec<String>,
    #[serde(rename = "lookupMs")]
    pub lookup_ms: u64,
    #[serde(rename = "connectMs")]
    pub connect_ms: Option<u64>,
    pub via: String,
}

/// Разобрать `api://<id>/...` или голый EndpointId.
#[cfg(feature = "p2p")]
fn parse_endpoint_id(node: &str) -> Result<iroh::EndpointId, String> {
    let raw = node.trim().strip_prefix("api://").unwrap_or(node.trim());
    let raw = raw.split('/').next().unwrap_or("");
    raw.parse::<iroh::EndpointId>().map_err(|e| format!("bad EndpointId: {e}"))
}

#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn discovery_probe(node: String) -> Result<DiscoveryReport, String> {
    let id = parse_endpoint_id(&node)?;
    Ok(p2p_impl::discovery_probe(id).await)
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn discovery_probe(_node: String) -> Result<DiscoveryReport, String> {
    Err("P2P is disabled in this build".to_string())
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn net_doctor_run(_app: tauri::AppHandle) -> Vec<DoctorCheck> {
    vec![DoctorCheck {
        id: "verdict".to_string(),
        status: "fail".to_string(),
        detail: "P2P is disabled in this build (rebuild with --features p2p)".to_string(),
    }]
}

/// v1.63.17: проверить пользовательские (n1) реле по чистому TCP/TLS 443
/// dual-stack (IPv4 + IPv6). Вызывается из UI кнопкой «Проверить мои реле»
/// рядом с полем ввода n1-relay в разделе Сеть.
#[cfg(feature = "p2p")]
#[tauri::command]
pub(crate) async fn net_doctor_check_relays(
    app: tauri::AppHandle,
    urls: Vec<String>,
) -> Vec<DoctorCheck> {
    p2p_impl::doctor_check_relays(&app, urls).await
}

#[cfg(not(feature = "p2p"))]
#[tauri::command]
pub(crate) async fn net_doctor_check_relays(
    _app: tauri::AppHandle,
    _urls: Vec<String>,
) -> Vec<DoctorCheck> {
    vec![DoctorCheck {
        id: "myrelay:disabled".to_string(),
        status: "fail".to_string(),
        detail: "P2P is disabled in this build (rebuild with --features p2p)".to_string(),
    }]
}
