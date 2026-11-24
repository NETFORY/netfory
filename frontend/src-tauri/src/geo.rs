//! Author: TechnoL0g / Project: SmartNet Core / Date: 2026-06-22
//! GeoLite2-City geolocation: one-time MMDB download + IP→coordinate lookup.
//!
//! The 3D node planet places connected peers at their real geographic position
//! instead of a deterministic hash fallback. The City database is fetched once
//! into `data/geo/GeoLite2-City.mmdb` (gunzipped from the public mirror) and
//! memory-mapped via `maxminddb`. Private / loopback / unknown IPs are skipped
//! so the frontend can fall back to its hashed placement for those peers.

use maxminddb::{geoip2, Reader};
use once_cell::sync::OnceCell;
use serde::Serialize;
use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

const GEO_DB_URL: &str =
    "https://github.com/technologiespro/GeoLite2-City/raw/refs/heads/master/GeoLite2-City.mmdb.gz";

static READER: OnceCell<Reader<Vec<u8>>> = OnceCell::new();

fn db_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = crate::ensure_data_dir(app).join("geo");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("GeoLite2-City.mmdb")
}

/// Download + gunzip the GeoLite2 City DB into data/geo (one-time, blocking).
fn download_db(path: &Path) -> Result<(), String> {
    let resp = ureq::get(GEO_DB_URL)
        .call()
        .map_err(|e| format!("download: {e}"))?;
    let mut compressed = Vec::new();
    resp.into_reader()
        .read_to_end(&mut compressed)
        .map_err(|e| format!("read: {e}"))?;
    let mut dec = flate2::read::GzDecoder::new(&compressed[..]);
    let mut raw = Vec::new();
    dec.read_to_end(&mut raw).map_err(|e| format!("gunzip: {e}"))?;
    if raw.len() < 1024 {
        return Err("decompressed db too small".into());
    }
    // атомарно: читатели никогда не видят недописанный файл
    let tmp = path.with_extension("mmdb.part");
    std::fs::write(&tmp, &raw).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Lazily download (if missing) + open the reader, caching it for the session.
fn ensure_reader(app: &tauri::AppHandle) -> Result<&'static Reader<Vec<u8>>, String> {
    if let Some(r) = READER.get() {
        return Ok(r);
    }
    // одна загрузка/открытие на процесс: параллельные вызовы ждут, а не читают 70 МБ каждый
    static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = READER.get() {
        return Ok(r);
    }
    let path = db_path(app);
    if !path.exists() {
        download_db(&path)?;
    }
    let reader = Reader::open_readfile(&path).map_err(|e| format!("open mmdb: {e}"))?;
    let _ = READER.set(reader); // first writer wins (race-safe)
    READER.get().ok_or_else(|| "reader init failed".to_string())
}

#[derive(Serialize)]
pub struct GeoPoint {
    pub ip: String,
    pub lat: f64,
    pub lng: f64,
}

/// Parse "1.2.3.4", "1.2.3.4:7374" or "[::1]:80" into an IpAddr.
fn parse_ip(addr: &str) -> Option<IpAddr> {
    let a = addr.trim();
    if a.is_empty() {
        return None;
    }
    if let Ok(ip) = a.parse::<IpAddr>() {
        return Some(ip);
    }
    // strip a [..] ipv6 bracket or a trailing :port for ipv4
    if a.starts_with('[') {
        if let Some(end) = a.find(']') {
            if let Ok(ip) = a[1..end].parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    if let Some((host, _)) = a.rsplit_once(':') {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Some(ip);
        }
    }
    None
}

/// Only routable, geolocatable addresses (skip LAN/loopback/link-local).
fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast())
        }
        IpAddr::V6(v) => !(v.is_loopback() || v.is_unspecified()),
    }
}

/// Has the City DB already been downloaded?
#[tauri::command]
pub async fn geo_db_ready(app: tauri::AppHandle) -> bool {
    db_path(&app).exists()
}

/// Trigger the one-time DB download / open (returns once ready).
#[tauri::command]
pub async fn ensure_geo_db(app: tauri::AppHandle) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || ensure_reader(&app).map(|_| true))
        .await
        .map_err(|e| e.to_string())?
}

/// Resolve a batch of addresses (ip or ip:port) to coordinates. Skips private
/// / loopback IPs and unknown lookups; returns only the entries that resolved.
#[tauri::command]
pub async fn geo_lookup(app: tauri::AppHandle, addrs: Vec<String>) -> Result<Vec<GeoPoint>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let reader = ensure_reader(&app)?;
        let mut out = Vec::new();
        for a in addrs {
            let Some(ip) = parse_ip(&a) else { continue };
            if !is_public(&ip) {
                continue;
            }
            if let Ok(res) = reader.lookup(ip) {
                if let Ok(Some(city)) = res.decode::<geoip2::City>() {
                    let loc = city.location;
                    if let (Some(lat), Some(lng)) = (loc.latitude, loc.longitude) {
                        out.push(GeoPoint { ip: a, lat, lng });
                    }
                }
            }
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// ISO-код страны для публичного ip/ip:port (v1.136.2). Без открытой базы → None.
pub(crate) fn country_iso(app: &tauri::AppHandle, addr: &str) -> Option<String> {
    if READER.get().is_none() && !db_path(app).exists() {
        return None;
    }
    let reader = ensure_reader(app).ok()?;
    let ip = parse_ip(addr).filter(is_public)?;
    let city = reader.lookup(ip).ok()?.decode::<geoip2::City>().ok()??;
    city.country.iso_code.map(|s| s.to_ascii_uppercase())
}
