//! Microsoft Defender exclusions for the app's folders (`Config.defender`)
//! are requested through a single UAC prompt at startup and when the app
//! requests them again (via a "fix it" button). The applied exclusions and
//! user refusals are stored in `<identifier>\defender.json` because reading
//! Defender's native list requires administrator privileges. Builds marked
//! for local testing never prompt, and a dev build ([`crate::dev`]) leaves
//! Defender alone: nothing is missing, and nothing is asked.

use crate::config;
use std::path::PathBuf;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct State {
    #[serde(default)]
    excluded: Vec<String>,
    #[serde(default)]
    declined: bool,
}

fn state_path() -> PathBuf {
    config().log_dir().with_file_name("defender.json")
}

fn load() -> State {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(s: &State) {
    let p = state_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, serde_json::to_string_pretty(s).unwrap_or_default());
}

fn norm(p: &str) -> String {
    p.trim_end_matches(['\\', '/']).to_lowercase()
}

fn wanted() -> Vec<String> {
    wanted_in(config().defender, crate::dev())
}

// The folders originate from the app and, in a dev build, may still be the
// installed copy's; a dev build never requests an elevated change.
fn wanted_in(folders: Option<fn() -> Vec<PathBuf>>, dev: bool) -> Vec<String> {
    if dev {
        return Vec::new();
    }
    folders
        .map(|f| f())
        .unwrap_or_default()
        .iter()
        .map(|p| p.display().to_string())
        .collect()
}

/// The configured folders Defender still scans, as far as the kit knows.
pub fn missing() -> Vec<String> {
    let s = load();
    wanted()
        .into_iter()
        .filter(|w| !s.excluded.iter().any(|e| norm(e) == norm(w)))
        .collect()
}

/// Defender has nothing to scan because everything is excluded, or it
/// isn't running.
pub fn excluded() -> bool {
    missing().is_empty() || !running()
}

/// The user dismissed the last prompt; startup won't ask again until the app
/// does, with `ensure(true)`.
pub fn declined() -> bool {
    load().declined
}

/// Folders an app excluded prior to using the kit, preventing them from
/// being requested again.
pub fn remember(paths: &[String]) {
    let mut s = load();
    for p in paths {
        if !s.excluded.iter().any(|e| norm(e) == norm(p)) {
            s.excluded.push(p.clone());
        }
    }
    save(&s);
}

/// Adds the missing exclusions (one UAC prompt). `asked`: the user asked, so
/// an earlier dismissal doesn't hold it back.
pub fn ensure(asked: bool) -> Result<(), String> {
    if config().test_mode() {
        return Ok(());
    }
    let missing = missing();
    if missing.is_empty() {
        return Ok(());
    }
    if !running() {
        return Err("Microsoft Defender is not running".into());
    }
    let mut s = load();
    if !asked && s.declined {
        return Err("exclusions were declined".into());
    }
    let result = add(&missing);
    match &result {
        Ok(()) => {
            crate::log(format!("defender: excluded {}", missing.join(", ")));
            s.excluded.extend(missing);
            s.declined = false;
        }
        Err(e) => {
            crate::log(format!("defender: {e}"));
            s.declined = true;
        }
    }
    save(&s);
    result
}

/// The Defender antivirus service is active (checked via the service
/// manager: instant, no admin, no PowerShell).
#[cfg(windows)]
pub fn running() -> bool {
    use windows_sys::Win32::System::Services::*;
    let name: Vec<u16> = "WinDefend".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return false;
        }
        let svc = OpenServiceW(scm, name.as_ptr(), SERVICE_QUERY_STATUS);
        let mut status: SERVICE_STATUS = std::mem::zeroed();
        let running =
            !svc.is_null() && QueryServiceStatus(svc, &mut status) != 0 && status.dwCurrentState == SERVICE_RUNNING;
        if !svc.is_null() {
            CloseServiceHandle(svc);
        }
        CloseServiceHandle(scm);
        running
    }
}

#[cfg(not(windows))]
pub fn running() -> bool {
    false
}

/// One elevated PowerShell instance adds each path.
#[cfg(windows)]
fn add(paths: &[String]) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let script: String = paths
        .iter()
        .map(|p| {
            format!(
                "Add-MpPreference -ExclusionPath '{}' -ErrorAction Stop;",
                p.replace('\'', "''")
            )
        })
        .collect();
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let elevated = format!(
        "$p = Start-Process powershell -Verb RunAs -PassThru -Wait -WindowStyle Hidden \
         -ArgumentList @('-NoProfile','-EncodedCommand','{}'); exit $p.ExitCode",
        base64(&utf16)
    );
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let out = std::process::Command::new(PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .args(["-NoProfile", "-Command", &elevated])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    if String::from_utf8_lossy(&out.stderr).contains("canceled by the user") {
        return Err("the Windows prompt was dismissed".into());
    }
    Err(format!(
        "adding exclusions failed (exit {:?}); is Microsoft Defender the active antivirus?",
        out.status.code()
    ))
}

#[cfg(not(windows))]
fn add(_paths: &[String]) -> Result<(), String> {
    Err("Defender exclusions are Windows only".into())
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                T[(n >> (18 - 6 * i)) as usize & 63] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }

    #[test]
    fn paths_compare_loosely() {
        assert_eq!(super::norm(r"C:\Games\"), super::norm(r"c:\games"));
    }

    #[test]
    fn a_dev_build_asks_for_nothing() {
        fn folders() -> Vec<std::path::PathBuf> {
            vec![std::path::PathBuf::from(r"C:\Games")]
        }
        assert!(super::wanted_in(Some(folders), true).is_empty());
        assert_eq!(super::wanted_in(Some(folders), false), vec![r"C:\Games".to_string()]);
        assert!(super::wanted_in(None, false).is_empty());
    }
}
