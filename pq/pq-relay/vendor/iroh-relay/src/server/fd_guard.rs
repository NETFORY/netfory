//! SmartNet patch: защита accept-циклов от исчерпания файловых дескрипторов (EMFILE/ENFILE).
//!
//! Без патча `listener.accept()` при «Too many open files» падает мгновенно и цикл
//! крутится на 100% CPU, заливая журнал (так завис relay-fsn7). Здесь:
//! * при EMFILE/ENFILE — пауза `BACKOFF`, чтобы отпустить CPU и дать соединениям закрыться;
//! * если ошибка держится дольше `FATAL_AFTER` подряд — процесс завершается с кодом 75
//!   (EX_TEMPFAIL): systemd (`Restart=always`) поднимет релей заново с чистыми fd.
//!   Это самолечение, а не исправление лимита: `LimitNOFILE` всё равно нужно поднять.
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BACKOFF: Duration = Duration::from_millis(250);
const FATAL_AFTER: Duration = Duration::from_secs(30);
const EXIT_TEMPFAIL: i32 = 75;

static FIRST_SEEN: Mutex<Option<Instant>> = Mutex::new(None);

fn is_fd_exhaustion(err: &std::io::Error) -> bool {
    matches!(err.raw_os_error(), Some(24) | Some(23)) // EMFILE / ENFILE
}

/// Вызывать из accept-цикла при `Err`. Для прочих ошибок — короткая пауза без счётчика.
pub(crate) async fn on_accept_error(err: &std::io::Error) {
    if !is_fd_exhaustion(err) {
        *FIRST_SEEN.lock().unwrap() = None;
        tokio::time::sleep(Duration::from_millis(20)).await;
        return;
    }
    let since = {
        let mut g = FIRST_SEEN.lock().unwrap();
        let t = g.get_or_insert_with(Instant::now);
        t.elapsed()
    };
    if since >= FATAL_AFTER {
        tracing::error!(
            "file descriptors exhausted for {}s (EMFILE) — exiting with code {} so the supervisor restarts the relay; raise LimitNOFILE",
            since.as_secs(),
            EXIT_TEMPFAIL
        );
        std::process::exit(EXIT_TEMPFAIL);
    }
    tokio::time::sleep(BACKOFF).await;
}

/// Успешный accept сбрасывает окно отсчёта.
pub(crate) fn on_accept_ok() {
    *FIRST_SEEN.lock().unwrap() = None;
}
