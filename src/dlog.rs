/// Lightweight debug logger — writes timestamped lines to .yggdra/debug.log.
/// Not for production use; never committed. Enable by just running the binary.
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Mutex<BufWriter<File>>> = OnceLock::new();

/// Open (or truncate) .yggdra/debug.log. Call once at startup before App::new.
pub fn init() {
    let _ = std::fs::create_dir_all(".yggdra");
    match OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(".yggdra/debug.log")
    {
        Ok(f) => {
            let _ = LOG.set(Mutex::new(BufWriter::new(f)));
            log("=== yggdra debug log started ===");
        }
        Err(e) => eprintln!("[dlog] failed to open debug.log: {e}"),
    }
}

/// Write a single timestamped line. No-op if init() was not called.
pub fn log(msg: &str) {
    let Some(lock) = LOG.get() else { return };
    let Ok(mut w) = lock.lock() else { return };
    let ts = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let s = (ms / 1000) % 86400;
        let hh = s / 3600;
        let mm = (s % 3600) / 60;
        let ss = s % 60;
        let millis = ms % 1000;
        format!("{hh:02}:{mm:02}:{ss:02}.{millis:03}")
    };
    let _ = writeln!(w, "{ts} | {msg}");
    let _ = w.flush();
}

/// Format and log — same as `log(format!(...).as_str())` but without an alloc when disabled.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        $crate::dlog::log(&format!($($arg)*))
    };
}
