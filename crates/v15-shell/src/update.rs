//! Checking, downloading, and verifying the update. Installation (the
//! swap and handover) is platform-specific: win.rs.

use crate::{log, splash, Config};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Runtime};
use v15_update::Candidate;

/// The maximum size of an update file (unpacked).
const MAX_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) const PLATFORM: Option<&str> = Some(v15_update::WINDOWS_X64);
#[cfg(not(all(windows, target_arch = "x86_64")))]
pub(crate) const PLATFORM: Option<&str> = None;

static UPDATING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// Another check is running.
    Busy,
    /// Not on this platform, a debug build, or not the installed copy.
    Unsupported,
    Failed(String),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::UpToDate => f.write_str("up-to-date"),
            Outcome::Busy => f.write_str("busy"),
            Outcome::Unsupported => f.write_str("unsupported"),
            Outcome::Failed(e) => write!(f, "failed: {e}"),
        }
    }
}

/// Checks now. A newer signed build is downloaded behind the splash and
/// handed over to, and this process exits; so this returns only when
/// nothing was installed. Blocking: call it off the main thread.
pub fn check<R: Runtime>(app: &AppHandle<R>) -> Outcome {
    let cfg = crate::config();
    let Some(platform) = PLATFORM else {
        return Outcome::Unsupported;
    };
    if !crate::active() || !crate::installed() {
        return Outcome::Unsupported;
    }
    if UPDATING.swap(true, Ordering::SeqCst) {
        return Outcome::Busy;
    }
    let outcome = run(app, cfg, platform);
    UPDATING.store(false, Ordering::SeqCst);
    if let Outcome::Failed(e) = &outcome {
        log(format!("update failed: {e}"));
    }
    outcome
}

pub(crate) fn start_loop<R: Runtime>(app: AppHandle<R>, cfg: &'static Config) {
    std::thread::spawn(move || {
        std::thread::sleep(cfg.first_check);
        loop {
            let _ = check(&app);
            std::thread::sleep(cfg.check_every);
        }
    });
}

fn run<R: Runtime>(app: &AppHandle<R>, cfg: &Config, platform: &str) -> Outcome {
    let url = cfg.stamp.test_manifest.unwrap_or(cfg.manifest_url);
    let text = match get(url, Duration::from_secs(20), 4 * 1024 * 1024, |_| {}) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => return Outcome::Failed(format!("{url}: {e}")),
    };
    let manifest = match v15_update::parse(&text) {
        Ok(m) => m,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    // A test key is counted only in a build stamped for local tests.
    let test_key = cfg.stamp.test_manifest.and(cfg.stamp.test_pubkey);
    let keys: Vec<_> = cfg
        .pubkeys
        .iter()
        .chain(test_key.as_ref())
        .filter_map(|k| v15_update::verifying_key(k).ok())
        .collect();
    #[cfg(windows)]
    let skip = crate::win::skipped(cfg);
    #[cfg(not(windows))]
    let skip: Vec<u64> = Vec::new();
    let candidate = match v15_update::select(&manifest, cfg.app, platform, &keys, cfg.stamp.build, &skip) {
        Ok(Some(c)) => c,
        Ok(None) => return Outcome::UpToDate,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    log(format!(
        "update {} -> {} ({})",
        cfg.stamp.build, candidate.build, candidate.version
    ));
    splash::show(app, cfg, &candidate.version);
    let bytes = download(app, &candidate).or_else(|e| {
        log(format!("download failed ({e}), retrying"));
        download(app, &candidate)
    });
    splash::close(app);
    let bytes = match bytes {
        Ok(b) => b,
        Err(e) => return Outcome::Failed(format!("download: {e}")),
    };
    #[cfg(windows)]
    {
        match crate::win::apply(app, cfg, &candidate, &bytes) {
            Ok(never) => match never {},
            Err(e) => Outcome::Failed(e),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = bytes;
        Outcome::Unsupported
    }
}

/// The signed file, from its brotli copy if the manifest lists one.
fn download<R: Runtime>(app: &AppHandle<R>, c: &Candidate) -> Result<Vec<u8>, String> {
    let a = &c.artifact;
    if a.size > MAX_BYTES {
        return Err(format!("{} bytes is too big", a.size));
    }
    splash::progress(app, 0.0);
    let (url, expect) = match &a.br {
        Some(p) => (p.url.as_str(), p.size),
        None => (a.url.as_str(), a.size),
    };
    let fetched = get(url, Duration::from_secs(900), expect, |got| {
        splash::progress(app, got as f64 / expect.max(1) as f64)
    })?;
    if fetched.len() as u64 != expect {
        return Err(format!("got {} bytes, the manifest says {expect}", fetched.len()));
    }
    let bytes = if a.br.is_some() {
        let mut out = Vec::with_capacity(a.size as usize);
        brotli_decompressor::Decompressor::new(&fetched[..], 64 * 1024)
            .take(a.size + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("unpacking: {e}"))?;
        out
    } else {
        fetched
    };
    if !v15_update::matches(&bytes, a) {
        return Err("not the signed file (size or sha256 differ)".into());
    }
    if PLATFORM == Some(v15_update::WINDOWS_X64) && !bytes.starts_with(b"MZ") {
        return Err("not an exe".into());
    }
    Ok(bytes)
}

/// GET into memory, refusing more than `limit` bytes.
fn get(url: &str, timeout: Duration, limit: u64, mut on_progress: impl FnMut(u64)) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url).timeout(timeout).call().map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader().take(limit + 1);
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut shown = 0;
    loop {
        let n = reader.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let pct = buf.len() as u64 * 100 / limit.max(1);
        if pct != shown {
            shown = pct;
            on_progress(buf.len() as u64);
        }
    }
    if buf.len() as u64 > limit {
        return Err(format!("more than {limit} bytes"));
    }
    Ok(buf)
}
