//! `<app_log_dir>/v15-shell.log`: the actions taken by the bootstrap and
//! updater, along with any panic. It rolls over to `.1` at 1 MiB.

use crate::Config;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX: u64 = 1024 * 1024;

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn init(cfg: &Config) {
    if PATH.set(cfg.log_dir().join("v15-shell.log")).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(format!("panic: {info}"));
        previous(info);
    }));
}

/// One line in the shell's log (and on stderr in debug builds).
pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    if cfg!(debug_assertions) {
        eprintln!("[v15-shell] {msg}");
    }
    let Some(path) = PATH.get() else { return };
    let _guard = LOCK.lock();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // one write per line: two processes append simultaneously during an
        // update
        let line = format!("{secs} [{}] {msg}\n", std::process::id());
        let _ = f.write_all(line.as_bytes());
    }
}
