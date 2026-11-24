//! Улики для «тихих» вылетов клиента (процесс умер без паники и без записи
//! SEH-фильтра). Пути смерти, которые обходят panic-hook и
//! SetUnhandledExceptionFilter:
//!   • OOM: аллокатор вернул null → handle_alloc_error → abort → __fastfail
//!     (fast-fail НЕ проходит через SEH-фильтр);
//!   • stack overflow: std ловит его своим VEH-обработчиком, печатает в stderr
//!     (невидимый на Windows) и делает abort → fast-fail;
//!   • heap corruption 0xC0000374, kill/TerminateProcess, выход по WM_ENDSESSION.
//!
//! Что делаем: (1) глобальный аллокатор-обёртка пишет ALLOCATION FAILURE с
//! памятью процесса; (2) first-chance VEH фиксирует stack overflow / heap
//! corruption ДО std; (3) heartbeat раз в 5 мин переписывает run-marker строкой
//! «uptime · RSS · commit · handles · GDI/USER» — при следующем старте после
//! грязного завершения последняя строка попадает в crash-лог (видно рост
//! памяти/хендлов перед смертью). Стоимость: один syscall в 5 минут.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

const HEARTBEAT_EVERY: Duration = Duration::from_secs(300);
/// Аллокация ≥ 64 МБ одним куском — редкость; считаем их отдельно (улика).
const BIG_ALLOC: usize = 64 * 1024 * 1024;

pub struct DiagAlloc;

static IN_OOM_REPORT: AtomicBool = AtomicBool::new(false);
/// Живая куча Rust (байты, выданные System-аллокатором и ещё не освобождённые).
/// RSS ≫ HEAP_LIVE ⇒ память утекает вне Rust-кучи (mmap sled/candle, драйвер,
/// хендлы); RSS ≈ HEAP_LIVE ⇒ утечка в наших/библиотечных структурах.
static HEAP_LIVE: AtomicUsize = AtomicUsize::new(0);
static HEAP_PEAK: AtomicUsize = AtomicUsize::new(0);
static BIG_ALLOCS: AtomicU64 = AtomicU64::new(0);
static LAST_BIG_ALLOC: AtomicUsize = AtomicUsize::new(0);
/// Ловушка: для аллокаций ≥ BIG_TRACE пишем backtrace в crash-лог (первые
/// BIG_TRACE_MAX за сессию). Один захват за раз — CAPTURING отсекает рекурсию
/// (сам захват/форматирование аллоцируют).
const BIG_TRACE: usize = 256 * 1024 * 1024;
const BIG_TRACE_MAX: u64 = 6;
static CAPTURING: AtomicBool = AtomicBool::new(false);
static TRACES_WRITTEN: AtomicU64 = AtomicU64::new(0);

#[inline]
fn note_alloc(size: usize) {
    let live = HEAP_LIVE.fetch_add(size, Ordering::Relaxed) + size;
    if live > HEAP_PEAK.load(Ordering::Relaxed) {
        HEAP_PEAK.store(live, Ordering::Relaxed);
    }
    if size >= BIG_ALLOC {
        BIG_ALLOCS.fetch_add(1, Ordering::Relaxed);
        LAST_BIG_ALLOC.store(size, Ordering::Relaxed);
        if size >= BIG_TRACE {
            trace_big_alloc(size, live);
        }
    }
}

#[cold]
fn trace_big_alloc(size: usize, live: usize) {
    if TRACES_WRITTEN.load(Ordering::Relaxed) >= BIG_TRACE_MAX {
        return;
    }
    if CAPTURING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let n = TRACES_WRITTEN.fetch_add(1, Ordering::Relaxed) + 1;
    let bt = std::backtrace::Backtrace::force_capture();
    // имя потока — часто само называет подсистему (ui-watchdog, media-seeders,
    // tokio-runtime-worker …); без PDB это главная зацепка
    let thread = std::thread::current().name().unwrap_or("<unnamed>").to_string();
    // якорь для ручной расшифровки адресов без PDB: offset = ip − anchor
    let anchor = trace_big_alloc as usize;
    crate::crash_log(&format!(
        "BIG ALLOC #{n}: {:.0} MB requested (heap live {:.0} MB) · thread «{thread}» · anchor trace_big_alloc={anchor:#x} — who:\n{bt}\nraw:\n{bt:#?}",
        size as f64 / 1e6,
        live as f64 / 1e6
    ));
    CAPTURING.store(false, Ordering::Release);
}

unsafe impl GlobalAlloc for DiagAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if p.is_null() {
            report_oom(l.size());
        } else {
            note_alloc(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if p.is_null() {
            report_oom(l.size());
        } else {
            note_alloc(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        HEAP_LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let n = unsafe { System.realloc(p, l, new_size) };
        if n.is_null() {
            report_oom(new_size);
        } else {
            HEAP_LIVE.fetch_sub(l.size(), Ordering::Relaxed);
            note_alloc(new_size);
        }
        n
    }
}

/// Пишем до abort'а; повторный вход (аллокации внутри crash_log) отсекаем.
fn report_oom(size: usize) {
    if IN_OOM_REPORT.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::crash_log(&format!(
        "ALLOCATION FAILURE: {} bytes requested ({:.1} MB) — process aborts (OOM / address space / fragmentation) · {}",
        size,
        size as f64 / 1e6,
        mem_summary()
    ));
    IN_OOM_REPORT.store(false, Ordering::SeqCst);
}

/// Снимок ресурсов процесса для Memory Guard (строка состояния) и crash-лога.
/// `alerts` — что вышло за безопасный порог («RSS 1620 MB», «GDI 7300»).
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot {
    pub rss_mb: u64,
    pub commit_mb: u64,
    pub peak_commit_mb: u64,
    pub handles: u32,
    pub gdi: u32,
    pub user: u32,
    /// живая Rust-куча / её пик (МБ) — см. HEAP_LIVE.
    pub heap_live_mb: u64,
    pub heap_peak_mb: u64,
    /// число аллокаций ≥ 64 МБ за время работы и размер последней (МБ).
    pub big_allocs: u64,
    pub last_big_alloc_mb: u64,
    /// перепись подсистем: («sled on-disk MB», n), («gossip outbox KB», n), …
    pub census: Vec<(String, u64)>,
    pub alerts: Vec<String>,
    /// диск повреждён (NTFS 1392/1393/CRC) и где это поймали.
    pub disk_corrupt: bool,
    pub disk_corrupt_where: String,
}

/// Дешёвая перепись in-memory реестров — куда именно растёт память. Только
/// счётчики/длины под короткими локами, без обхода тяжёлых структур.
fn census() -> Vec<(String, u64)> {
    let mut c: Vec<(String, u64)> = Vec::new();
    c.push(("sled on-disk MB".into(), crate::DB.size_on_disk().unwrap_or(0) / 1_000_000));
    c.push(("syslog lines".into(), crate::syslog::line_count() as u64));
    c.push(("p2p peers".into(), crate::p2p::PEERS.lock().map(|m| m.len()).unwrap_or(0) as u64));
    c.push(("private dApp RAM MB".into(), crate::private_dapp::ram_bytes() / 1_000_000));
    c.push(("translator engine".into(), crate::translator::engine_loaded() as u64));
    c.push(("torrents managed".into(), crate::torrent::managed_count() as u64));
    #[cfg(feature = "p2p")]
    {
        let (items, bytes) = crate::chat::gossip::outbox_size();
        c.push(("gossip outbox items".into(), items as u64));
        c.push(("gossip outbox KB".into(), (bytes / 1024) as u64));
        c.push(("gossip topics".into(), crate::chat::gossip::topic_count() as u64));
    }
    c
}

/// Пороги. GDI/USER: жёсткий лимит Windows — 10 000 объектов на процесс, после
/// него CreateWindow/CreateBitmap молча падают (типичный «тихий» вылет WebView2).
/// Хендлы: у здорового клиента < 2000; рост к 5000 = утечка сокетов/файлов.
const RSS_WARN_MB: u64 = 1536;
const COMMIT_WARN_MB: u64 = 2560;
const HANDLES_WARN: u32 = 5000;
const GDI_WARN: u32 = 7000;
const USER_WARN: u32 = 7000;

pub(crate) fn snapshot() -> ResourceSnapshot {
    let mut s = ResourceSnapshot::default();
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetGuiResources, GetProcessHandleCount};
        unsafe {
            let h = GetCurrentProcess();
            let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            if K32GetProcessMemoryInfo(h, &mut pmc, pmc.cb) != 0 {
                s.rss_mb = (pmc.WorkingSetSize as f64 / 1e6) as u64;
                s.commit_mb = (pmc.PagefileUsage as f64 / 1e6) as u64;
                s.peak_commit_mb = (pmc.PeakPagefileUsage as f64 / 1e6) as u64;
            }
            GetProcessHandleCount(h, &mut s.handles);
            s.gdi = GetGuiResources(h, 0);
            s.user = GetGuiResources(h, 1);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(pages) = std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|st| st.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()))
        {
            s.rss_mb = (pages as f64 * 4096.0 / 1e6) as u64;
        }
        if let Ok(rd) = std::fs::read_dir("/proc/self/fd") {
            s.handles = rd.count() as u32;
        }
    }
    if s.rss_mb >= RSS_WARN_MB {
        s.alerts.push(format!("RSS {} MB", s.rss_mb));
    }
    if s.commit_mb >= COMMIT_WARN_MB {
        s.alerts.push(format!("commit {} MB", s.commit_mb));
    }
    if s.handles >= HANDLES_WARN {
        s.alerts.push(format!("handles {}", s.handles));
    }
    if s.gdi >= GDI_WARN {
        s.alerts.push(format!("GDI {}", s.gdi));
    }
    if s.user >= USER_WARN {
        s.alerts.push(format!("USER {}", s.user));
    }
    s.heap_live_mb = (HEAP_LIVE.load(Ordering::Relaxed) as f64 / 1e6) as u64;
    s.heap_peak_mb = (HEAP_PEAK.load(Ordering::Relaxed) as f64 / 1e6) as u64;
    s.big_allocs = BIG_ALLOCS.load(Ordering::Relaxed);
    s.last_big_alloc_mb = (LAST_BIG_ALLOC.load(Ordering::Relaxed) as f64 / 1e6) as u64;
    if !s.alerts.is_empty() {
        // при тревоге добавляем в текст плашки главную улику: где память
        s.alerts.push(format!("heap {} MB", s.heap_live_mb));
    }
    s.census = census();
    let (dc, dw) = disk_corrupt();
    s.disk_corrupt = dc;
    s.disk_corrupt_where = dw;
    s
}

/// «RSS 812 MB · commit 1.3 GB · peak 1.9 GB · heap 600 MB · handles 1200 · GDI 340 · USER 90 · [census]»
pub(crate) fn mem_summary() -> String {
    let s = snapshot();
    let census: Vec<String> = s.census.iter().map(|(k, v)| format!("{k}={v}")).collect();
    #[cfg(windows)]
    {
        format!(
            "RSS {} MB · commit {} MB · peak commit {} MB · heap {} MB (peak {}, big allocs {}, last {} MB) · handles {} · GDI {} · USER {} · [{}]",
            s.rss_mb, s.commit_mb, s.peak_commit_mb, s.heap_live_mb, s.heap_peak_mb, s.big_allocs,
            s.last_big_alloc_mb, s.handles, s.gdi, s.user, census.join(", ")
        )
    }
    #[cfg(not(windows))]
    {
        format!(
            "RSS {} MB · heap {} MB (peak {}) · fds {} · [{}]",
            s.rss_mb, s.heap_live_mb, s.heap_peak_mb, s.handles, census.join(", ")
        )
    }
}

static GUARD_ALERTING: AtomicBool = AtomicBool::new(false);

/// Диск повреждён: любой модуль, поймавший NTFS-ошибку 1392/1393/23 (файл или
/// папка повреждены, CRC), отмечает её здесь → строка состояния показывает
/// «диск требует проверки (chkdsk)». Хранит unix-время последней ошибки.
static DISK_CORRUPT_TS: AtomicU64 = AtomicU64::new(0);
static DISK_CORRUPT_WHERE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Вызывать на каждом IO-Result, где повреждение диска реально (запись
/// моделей/пакетов/медиа). Возвращает true, если это ошибка повреждения.
pub(crate) fn note_io_error(e: &std::io::Error, where_: &str) -> bool {
    let corrupt = matches!(e.raw_os_error(), Some(1392) | Some(1393) | Some(23));
    if corrupt {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let first = DISK_CORRUPT_TS.swap(ts, Ordering::Relaxed) == 0;
        if let Ok(mut w) = DISK_CORRUPT_WHERE.lock() {
            *w = where_.to_string();
        }
        if first {
            let msg = format!("DISK CORRUPTION at {where_}: {e} — run `chkdsk /f` on this drive");
            crate::crash_log(&msg);
            crate::syslog::log("sys", msg);
        }
    }
    corrupt
}

pub(crate) fn disk_corrupt() -> (bool, String) {
    let ts = DISK_CORRUPT_TS.load(Ordering::Relaxed);
    let w = DISK_CORRUPT_WHERE.lock().map(|w| w.clone()).unwrap_or_default();
    (ts > 0, w)
}

/// Memory Guard для строки состояния: снимок ресурсов + список превышений.
/// Переходы «норма → превышение → норма» один раз пишем в системную консоль.
#[tauri::command(async)]
pub(crate) fn get_process_resources() -> ResourceSnapshot {
    let s = snapshot();
    let alerting = !s.alerts.is_empty();
    if alerting != GUARD_ALERTING.swap(alerting, Ordering::SeqCst) {
        if alerting {
            let msg = format!("MEMORY GUARD: resources above safe limit — {} · {}", s.alerts.join(" · "), mem_summary());
            crate::crash_log(&msg);
            crate::syslog::log("sys", msg);
        } else {
            crate::syslog::log("sys", format!("memory guard: back to normal · {}", mem_summary()));
        }
    }
    s
}

/// First-chance VEH: stack overflow и heap corruption фиксируем ДО того, как
/// std/ntdll убьют процесс fast-fail'ом (мимо SetUnhandledExceptionFilter).
/// Только фатальные коды — first-chance видит и штатно обрабатываемые
/// исключения (C++ внутри WebView2 и т.п.), их не трогаем.
#[cfg(windows)]
pub(crate) fn install_veh() {
    use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};
    unsafe extern "system" fn veh(info: *mut EXCEPTION_POINTERS) -> i32 {
        let (code, addr) = unsafe {
            if info.is_null() || (*info).ExceptionRecord.is_null() {
                (0u32, 0usize)
            } else {
                let rec = (*info).ExceptionRecord;
                ((*rec).ExceptionCode as u32, (*rec).ExceptionAddress as usize)
            }
        };
        let what = match code {
            0xC00000FD => Some("STACK OVERFLOW (deep recursion / huge stack buffer in a thread)"),
            0xC0000374 => Some("HEAP CORRUPTION (use-after-free / double free / buffer overrun in native code)"),
            0xC000001D => Some("ILLEGAL INSTRUCTION"),
            0xC0000006 => Some("IN-PAGE ERROR (backing file/disk unreadable while paging)"),
            _ => None,
        };
        if let Some(w) = what {
            let thread = std::thread::current().name().unwrap_or("<unnamed>").to_string();
            crate::crash_log(&format!(
                "NATIVE FATAL 0x{code:08X} at 0x{addr:X} in thread '{thread}': {w} · {}",
                mem_summary()
            ));
        }
        0 // EXCEPTION_CONTINUE_SEARCH
    }
    unsafe {
        AddVectoredExceptionHandler(1, Some(veh));
    }
}

#[cfg(not(windows))]
pub(crate) fn install_veh() {}

/// Строка heartbeat'а в run-marker: `v<ver> · up 3h12m · RSS … · threads N`.
fn heartbeat_line(started: std::time::Instant) -> String {
    let up = started.elapsed().as_secs();
    format!(
        "v{} · up {}h{:02}m · {} · {}",
        env!("CARGO_PKG_VERSION"),
        up / 3600,
        (up % 3600) / 60,
        mem_summary(),
        chrono_like_now()
    )
}

fn chrono_like_now() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("ts {ts}")
}

/// Последняя строка heartbeat'а прошлой сессии (из run-marker) — улика при
/// грязном завершении. Пустая строка, если маркер старого формата.
pub(crate) fn last_heartbeat(marker: &std::path::Path) -> String {
    std::fs::read_to_string(marker)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| s.contains("up "))
        .unwrap_or_default()
}

/// Фоновый heartbeat: раз в 5 минут переписывает run-marker. Первый снимок —
/// через 60 с после старта (видна базовая линия памяти).
pub(crate) fn spawn_heartbeat() {
    let marker = crate::run_marker_path();
    let started = std::time::Instant::now();
    std::thread::Builder::new()
        .name("heartbeat".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(60));
            loop {
                if !marker.exists() {
                    // маркер снят = штатный выход в процессе, больше не пишем
                    return;
                }
                let line = heartbeat_line(started);
                // Растущая память — пишем кривую роста в постоянный crash-лог
                // (маркер при штатном перезапуске стирается, улика пропадала бы).
                if HEAP_LIVE.load(Ordering::Relaxed) as u64 / 1_000_000 >= RSS_WARN_MB / 2
                    || snapshot().rss_mb >= RSS_WARN_MB
                {
                    crate::crash_log(&format!("MEMORY GUARD heartbeat: {line}"));
                }
                let _ = std::fs::write(&marker, line);
                std::thread::sleep(HEARTBEAT_EVERY);
            }
        })
        .ok();
}
