// ============================================================
//  ai_telemetry::hw — универсальный сбор характеристик железа
//
//  GPU: nvidia-smi (NVML-обёртка, кроссплатформенно) → AMD sysfs (Linux)
//       → system_profiler (macOS, вкл. unified memory Apple Silicon)
//       → Win32_VideoController (Windows fallback).
//  CPU/RAM: sysinfo + std::thread::available_parallelism.
// ============================================================

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub model: String,
    pub vram_total_mb: u64,
    pub vram_free_mb: u64,
    pub tflops_fp16: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuInfo {
    pub threads: u32,
    pub ram_total_mb: u64,
    pub ram_free_mb: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HwSnapshot {
    pub gpu: Option<GpuInfo>,
    pub cpu: CpuInfo,
}

/// Полный снимок железа (блокирующий — вызывать через spawn_blocking).
pub fn collect() -> HwSnapshot {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let ram_total_mb = sys.total_memory() / 1024 / 1024;
    let ram_free_mb = sys.available_memory() / 1024 / 1024;
    let threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1);
    HwSnapshot {
        gpu: detect_gpu(ram_total_mb),
        cpu: CpuInfo { threads, ram_total_mb, ram_free_mb },
    }
}

fn detect_gpu(ram_total_mb: u64) -> Option<GpuInfo> {
    if let Some(g) = nvidia_smi() {
        return Some(g);
    }
    #[cfg(target_os = "linux")]
    if let Some(g) = amd_sysfs() {
        return Some(g);
    }
    #[cfg(target_os = "macos")]
    if let Some(g) = macos_gpu(ram_total_mb) {
        return Some(g);
    }
    #[cfg(windows)]
    if let Some(g) = windows_wmi() {
        return Some(g);
    }
    let _ = ram_total_mb;
    None
}

/// Nvidia: официальный NVML-инструмент, есть на любой машине с драйвером.
fn nvidia_smi() -> Option<GpuInfo> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args(["--query-gpu=name,memory.total,memory.free", "--format=csv,noheader,nounits"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    let mut parts = line.split(',').map(|s| s.trim());
    let model = parts.next()?.to_string();
    let total: u64 = parts.next()?.parse().ok()?;
    let free: u64 = parts.next()?.parse().ok()?;
    if model.is_empty() || total == 0 {
        return None;
    }
    Some(GpuInfo {
        tflops_fp16: estimate_tflops(&model, total),
        model,
        vram_total_mb: total,
        vram_free_mb: free,
    })
}

/// AMD (Linux): amdgpu экспортирует VRAM-счётчики прямо в sysfs.
#[cfg(target_os = "linux")]
fn amd_sysfs() -> Option<GpuInfo> {
    for i in 0..8 {
        let base = format!("/sys/class/drm/card{i}/device");
        let Some(total) = std::fs::read_to_string(format!("{base}/mem_info_vram_total"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
        else {
            continue;
        };
        // интегрированные/сервисные < 64 MB пропускаем
        if total < 64 * 1024 * 1024 {
            continue;
        }
        let used = std::fs::read_to_string(format!("{base}/mem_info_vram_used"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let total_mb = total / 1024 / 1024;
        let free_mb = total.saturating_sub(used) / 1024 / 1024;
        let model = format!("AMD GPU ({} GB VRAM)", (total_mb + 512) / 1024);
        return Some(GpuInfo {
            tflops_fp16: estimate_tflops(&model, total_mb),
            model,
            vram_total_mb: total_mb,
            vram_free_mb: free_mb,
        });
    }
    None
}

/// macOS: дискретный VRAM либо unified memory Apple Silicon (~2/3 RAM доступно GPU).
#[cfg(target_os = "macos")]
fn macos_gpu(ram_total_mb: u64) -> Option<GpuInfo> {
    let out = std::process::Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = v["SPDisplaysDataType"].as_array()?;
    let g = arr.first()?;
    let model = g["sppci_model"].as_str().unwrap_or("Apple GPU").to_string();
    let vram_mb = g["spdisplays_vram"]
        .as_str()
        .and_then(|s| s.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
        .map(|gb| gb * 1024)
        .unwrap_or(ram_total_mb * 2 / 3);
    Some(GpuInfo {
        tflops_fp16: estimate_tflops(&model, vram_mb),
        model,
        vram_total_mb: vram_mb,
        vram_free_mb: vram_mb / 2,
    })
}

/// Windows fallback (не-Nvidia): имя + AdapterRAM через CIM.
#[cfg(windows)]
fn windows_wmi() -> Option<GpuInfo> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Select-Object Name,AdapterRAM | ConvertTo-Json -Compress",
        ])
        .creation_flags(0x08000000)
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let list: Vec<serde_json::Value> = if v.is_array() {
        v.as_array()?.clone()
    } else {
        vec![v]
    };
    let mut best: Option<(String, u64)> = None;
    for g in &list {
        let name = g["Name"].as_str().unwrap_or("").to_string();
        let ram = g["AdapterRAM"].as_u64().unwrap_or(0) / 1024 / 1024;
        if name.is_empty() {
            continue;
        }
        if best.as_ref().map(|(_, r)| ram > *r).unwrap_or(true) {
            best = Some((name, ram));
        }
    }
    let (model, vram_mb) = best?;
    Some(GpuInfo {
        tflops_fp16: estimate_tflops(&model, vram_mb),
        model,
        vram_total_mb: vram_mb,
        vram_free_mb: vram_mb / 2,
    })
}

/// Оценка FP16 TFLOPS по модели (таблица популярных карт, длинные ключи выше).
const TFLOPS_TABLE: &[(&str, f64)] = &[
    ("rtx 5090", 209.0),
    ("rtx 5080", 112.0),
    ("rtx 5070 ti", 88.0),
    ("rtx 5070", 62.0),
    ("rtx 5060", 38.0),
    ("rtx 4090", 82.6),
    ("rtx 4080", 48.7),
    ("rtx 4070 ti", 40.1),
    ("rtx 4070", 29.2),
    ("rtx 4060 ti", 22.1),
    ("rtx 4060", 15.1),
    ("rtx 3090 ti", 40.0),
    ("rtx 3090", 35.6),
    ("rtx 3080 ti", 34.1),
    ("rtx 3080", 29.8),
    ("rtx 3070", 20.3),
    ("rtx 3060 ti", 16.2),
    ("rtx 3060", 12.7),
    ("rtx 3050", 9.1),
    ("rtx 2080 ti", 26.9),
    ("rtx 2080", 20.1),
    ("rtx 2070", 14.9),
    ("rtx 2060", 12.9),
    ("gtx 1080 ti", 11.3),
    ("gtx 1080", 8.9),
    ("gtx 1070", 6.5),
    ("gtx 1660", 5.0),
    ("gtx 1650", 3.0),
    ("rx 7900 xtx", 122.8),
    ("rx 7900", 103.0),
    ("rx 7800", 74.7),
    ("rx 7700", 70.3),
    ("rx 6900", 46.1),
    ("rx 6800", 41.5),
    ("rx 6700", 26.4),
    ("rx 6600", 17.9),
    ("m4 max", 34.0),
    ("m4 pro", 17.0),
    ("m4", 9.2),
    ("m3 max", 28.4),
    ("m3 pro", 14.0),
    ("m3", 8.2),
    ("m2 ultra", 54.2),
    ("m2 max", 27.2),
    ("m2 pro", 13.6),
    ("m2", 7.1),
    ("m1 ultra", 42.0),
    ("m1 max", 21.0),
    ("m1 pro", 10.4),
    ("m1", 5.2),
];

fn estimate_tflops(model: &str, vram_mb: u64) -> f64 {
    let m = model.to_lowercase();
    for (k, t) in TFLOPS_TABLE {
        if m.contains(k) {
            return *t;
        }
    }
    // грубая эвристика для неизвестных карт: ~2 TFLOPS FP16 на 1 GB VRAM
    ((vram_mb as f64 / 1024.0) * 2.0 * 10.0).round() / 10.0
}
