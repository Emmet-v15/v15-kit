//! Windows bootstrap: installation, running a single instance,
//! uninstallation, and applying an update by transferring control to it.
//!
//! Handover: the running build (A) writes the new exe beside itself,
//! renames itself to `<stem>.old`, places the new one in its position,
//! releases the single-instance mutex, and starts it with
//! `--v15-handoff=<A's pid>`, then hides its windows and waits. The new
//! build (B) sets A's "healthy" event once one of its pages has loaded (or,
//! with no window, once it has run for a few seconds); A then exits, and B
//! deletes `<stem>.old`. If B exits or a minute passes first, A kills B,
//! restores itself, records B's build as one to skip (`update-state.json`),
//! and continues.

use crate::{log, Config};
use std::convert::Infallible;
use std::fs;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};
use v15_update::Candidate;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, OpenProcess, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    EVENT_MODIFY_STATE, INFINITE, PROCESS_SYNCHRONIZE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};

const NO_WINDOW: u32 = 0x0800_0000;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
/// When there is no window to load, a new build is considered healthy
/// after this duration.
const WINDOWLESS_HEALTHY_AFTER: Duration = Duration::from_secs(8);

static SINGLETON: AtomicIsize = AtomicIsize::new(0);
/// Set in the build an update started: the pid of the build it replaces.
static PREDECESSOR: AtomicU32 = AtomicU32::new(0);
static SIGNALLED: AtomicBool = AtomicBool::new(false);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn names(cfg: &Config) -> (String, String, String) {
    let scope = if cfg.install.machine() { "Global" } else { "Local" };
    (
        format!("{scope}\\v15-{}-instance", cfg.app),
        format!("Local\\v15-{}-show", cfg.app),
        format!("Local\\v15-{}-quit", cfg.app),
    )
}

fn healthy_event(cfg: &Config, pid: u32) -> String {
    format!("Local\\v15-{}-healthy-{pid}", cfg.app)
}

fn stem(cfg: &Config) -> &str {
    cfg.exe.strip_suffix(".exe").unwrap_or(cfg.exe)
}

fn sibling(cfg: &Config, ext: &str) -> PathBuf {
    cfg.install_dir().join(format!("{}.{ext}", stem(cfg)))
}

fn hidden(cmd: &mut Command) -> &mut Command {
    cmd.creation_flags(NO_WINDOW)
}

fn message(title: &str, text: &str) {
    let (t, c) = (wide(text), wide(title));
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONINFORMATION);
    }
}

// ---------- single instance ----------

/// False when another instance holds the mutex (it has been asked to show).
fn acquire(cfg: &Config) -> bool {
    let (mutex, show, _) = names(cfg);
    let name = wide(&mutex);
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() {
            return true; // can't tell: run rather than refuse
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(h);
            signal(&show);
            return false;
        }
        SINGLETON.store(h as isize, Ordering::SeqCst);
    }
    true
}

fn release() {
    let h = SINGLETON.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe {
            CloseHandle(h as HANDLE);
        }
    }
}

fn signal(name: &str) -> bool {
    let n = wide(name);
    unsafe {
        let e = OpenEventW(EVENT_MODIFY_STATE, 0, n.as_ptr());
        if e.is_null() {
            return false;
        }
        SetEvent(e);
        CloseHandle(e);
    }
    true
}

fn held(cfg: &Config) -> bool {
    let name = wide(&names(cfg).0);
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        let existed = GetLastError() == ERROR_ALREADY_EXISTS;
        if !h.is_null() {
            CloseHandle(h);
        }
        existed
    }
}

/// "Show yourself" (second launch) and "quit" (uninstall).
fn watch_events<R: Runtime>(app: AppHandle<R>, cfg: &Config) {
    let (_, show, quit) = names(cfg);
    thread::spawn(move || unsafe {
        let (s, q) = (wide(&show), wide(&quit));
        let handles = [
            CreateEventW(std::ptr::null(), 0, 0, s.as_ptr()),
            CreateEventW(std::ptr::null(), 0, 0, q.as_ptr()),
        ];
        if handles.iter().any(|h| h.is_null()) {
            return;
        }
        loop {
            match WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) {
                r if r == WAIT_OBJECT_0 => {
                    let a = app.clone();
                    let _ = app.run_on_main_thread(move || crate::show(&a));
                }
                r if r == WAIT_OBJECT_0 + 1 => {
                    app.exit(0);
                    return;
                }
                _ => return,
            }
        }
    });
}

// ---------- before the GUI ----------

pub(crate) fn pre_gui(cfg: &'static Config) {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--uninstall") {
        uninstall(cfg);
        std::process::exit(0);
    }
    let predecessor = args
        .iter()
        .find_map(|a| a.strip_prefix("--v15-handoff="))
        .and_then(|p| p.parse().ok());
    if let Some(pid) = predecessor {
        PREDECESSOR.store(pid, Ordering::SeqCst);
    }
    if !acquire(cfg) {
        std::process::exit(0);
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let dir = cfg.install_dir();
    if exe.parent().is_some_and(|p| crate::same_dir(p, &dir)) {
        if predecessor.is_none() {
            clean_leftovers(cfg);
        }
        return;
    }
    match install(cfg, &exe, &dir) {
        Ok(target) => {
            log(format!("installed {} from {}", target.display(), exe.display()));
            release();
            let _ = Command::new(&target).args(&args[1..]).spawn();
            std::process::exit(0);
        }
        // it still functions from its current location, just not installed
        // (or updating)
        Err(e) => log(format!("install failed, running from {}: {e}", exe.display())),
    }
}

fn clean_leftovers(cfg: &Config) {
    for ext in ["old", "new", "failed"] {
        let p = sibling(cfg, ext);
        for _ in 0..5 {
            if !p.exists() || fs::remove_file(&p).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(400));
        }
    }
}

fn install(cfg: &Config, exe: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = dir.join(cfg.exe);
    if target.exists() {
        let old = sibling(cfg, "old");
        let _ = fs::remove_file(&old);
        fs::rename(&target, &old)?;
    }
    if fs::rename(exe, &target).is_err() {
        fs::copy(exe, &target)?; // another drive: the download stays behind
    }
    // downloaded: remove the Mark of the Web so the installed copy starts
    // quietly
    let _ = fs::remove_file(format!("{}:Zone.Identifier", target.display()));
    if cfg.test_root().is_none() {
        shortcuts(cfg, &target);
        register_uninstall(cfg, &target, dir);
    }
    Ok(target)
}

fn powershell(script: &str) {
    let _ = hidden(&mut Command::new("powershell.exe"))
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output();
}

fn shortcut_dirs(cfg: &Config) -> String {
    let programs = if cfg.install.machine() {
        "CommonPrograms"
    } else {
        "Programs"
    };
    if cfg.desktop_shortcut {
        format!("@([Environment]::GetFolderPath('{programs}'), [Environment]::GetFolderPath('Desktop'))")
    } else {
        format!("@([Environment]::GetFolderPath('{programs}'))")
    }
}

fn ps_quote(s: &str) -> String {
    s.replace('\'', "''")
}

fn shortcuts(cfg: &Config, target: &Path) {
    let t = ps_quote(&target.display().to_string());
    let d = ps_quote(&target.parent().map(|p| p.display().to_string()).unwrap_or_default());
    let name = ps_quote(cfg.name);
    powershell(&format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         foreach ($dir in {dirs}) {{ \
           $s = $ws.CreateShortcut((Join-Path $dir '{name}.lnk')); $s.TargetPath = '{t}'; \
           $s.WorkingDirectory = '{d}'; $s.IconLocation = '{t},0'; $s.Save() }}",
        dirs = shortcut_dirs(cfg)
    ));
}

fn uninstall_key(cfg: &Config) -> String {
    let root = if cfg.install.machine() { "HKLM" } else { "HKCU" };
    let name = match cfg.uninstall_entry {
        "" => format!("v15-{}", cfg.app),
        entry => entry.to_string(),
    };
    format!(r"{root}\Software\Microsoft\Windows\CurrentVersion\Uninstall\{name}")
}

fn reg(args: &[&str]) {
    let _ = hidden(&mut Command::new("reg.exe")).args(args).output();
}

fn register_uninstall(cfg: &Config, target: &Path, dir: &Path) {
    let exe = target.display().to_string();
    let uninstall = format!("\"{exe}\" --uninstall");
    let size_kb = fs::metadata(target).map(|m| m.len()).unwrap_or(0) / 1024;
    let key = uninstall_key(cfg);
    for (name, kind, value) in [
        ("DisplayName", "REG_SZ", cfg.name.to_string()),
        ("DisplayIcon", "REG_SZ", format!("{exe},0")),
        ("Publisher", "REG_SZ", cfg.publisher.to_string()),
        ("DisplayVersion", "REG_SZ", cfg.version.to_string()),
        ("InstallLocation", "REG_SZ", dir.display().to_string()),
        ("UninstallString", "REG_SZ", uninstall.clone()),
        ("QuietUninstallString", "REG_SZ", uninstall),
        ("NoModify", "REG_DWORD", "1".to_string()),
        ("NoRepair", "REG_DWORD", "1".to_string()),
        ("EstimatedSize", "REG_DWORD", size_kb.to_string()),
    ] {
        reg(&["add", &key, "/v", name, "/t", kind, "/d", &value, "/f"]);
    }
}

fn uninstall(cfg: &Config) {
    let (_, _, quit) = names(cfg);
    if signal(&quit) {
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(200));
            if !held(cfg) {
                break;
            }
        }
    }
    (cfg.on_uninstall)();
    if cfg.test_root().is_none() {
        reg(&["delete", &uninstall_key(cfg), "/f"]);
        powershell(&format!(
            "foreach ($dir in {dirs}) {{ Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $dir '{name}.lnk') }}",
            dirs = shortcut_dirs(cfg),
            name = ps_quote(cfg.name)
        ));
    }
    if cfg.remove_data_on_uninstall && cfg.test_root().is_none() {
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let _ = fs::remove_dir_all(PathBuf::from(base).join(cfg.identifier));
            }
        }
    }
    // The message comes first: the box blocks, and the folder can only go
    // once this process (which runs from it) has exited, which happens
    // right after.
    if cfg.test_root().is_none() {
        message(cfg.name, &format!("{} was removed from this computer.", cfg.name));
    }
    log("uninstalled");
    // Passed raw: Rust would quote it as a single argument with \" inside,
    // which cmd does not unescape, so rmdir would get a mangled path
    let dir = cfg.install_dir();
    let _ = hidden(&mut Command::new("cmd.exe"))
        .arg("/C")
        .raw_arg(format!("ping 127.0.0.1 -n 3 > nul & rmdir /S /Q \"{}\"", dir.display()))
        .spawn();
}

// ---------- after the GUI ----------

pub(crate) fn post_gui<R: Runtime>(app: &AppHandle<R>, cfg: &'static Config) {
    watch_events(app.clone(), cfg);
    let pid = PREDECESSOR.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let app = app.clone();
    thread::spawn(move || {
        thread::sleep(WINDOWLESS_HEALTHY_AFTER);
        if app.webview_windows().is_empty() {
            mark_healthy();
        }
    });
    thread::spawn(move || {
        // the build this one replaced exits once it sees us healthy
        unsafe {
            let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if !h.is_null() {
                WaitForSingleObject(h, (HANDOFF_TIMEOUT.as_millis() as u32) * 2);
                CloseHandle(h);
            }
        }
        clean_leftovers(cfg);
    });
}

/// Tells the build that started this one that it can exit. Called when a
/// page loads; does nothing in a build that wasn't started by an update.
pub(crate) fn mark_healthy() {
    let pid = PREDECESSOR.load(Ordering::SeqCst);
    if pid == 0 || SIGNALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let ok = signal(&healthy_event(crate::config(), pid));
    log(format!(
        "took over from {pid}{}",
        if ok { "" } else { " (it was gone)" }
    ));
}

// ---------- installing an update ----------

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct State {
    #[serde(default)]
    skip: Vec<u64>,
}

fn state_path(cfg: &Config) -> PathBuf {
    cfg.install_dir().join("update-state.json")
}

/// Builds that failed to take over on this machine.
pub(crate) fn skipped(cfg: &Config) -> Vec<u64> {
    fs::read_to_string(state_path(cfg))
        .ok()
        .and_then(|s| serde_json::from_str::<State>(&s).ok())
        .unwrap_or_default()
        .skip
}

fn skip(cfg: &Config, build: u64) {
    let mut s = State { skip: skipped(cfg) };
    s.skip.push(build);
    s.skip.sort_unstable();
    s.skip.dedup();
    let _ = fs::write(state_path(cfg), serde_json::to_string(&s).unwrap_or_default());
}

fn retry(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut last = Ok(());
    for _ in 0..10 {
        last = f();
        if last.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(300));
    }
    last
}

/// Replace `bytes` (verified) and transfer control to it. The process
/// exits once the new build assumes control; if it does not, the function
/// returns the reason.
pub(crate) fn apply<R: Runtime>(
    app: &AppHandle<R>,
    cfg: &Config,
    c: &Candidate,
    bytes: &[u8],
) -> Result<Infallible, String> {
    let exe = cfg.install_dir().join(cfg.exe);
    let (new, old, failed) = (sibling(cfg, "new"), sibling(cfg, "old"), sibling(cfg, "failed"));
    fs::write(&new, bytes).map_err(|e| format!("writing {}: {e}", new.display()))?;
    let _ = fs::remove_file(&old);
    if let Err(e) = fs::rename(&exe, &old) {
        let _ = fs::remove_file(&new);
        return Err(format!("moving the running exe aside: {e}"));
    }
    if let Err(e) = fs::rename(&new, &exe) {
        let _ = fs::rename(&old, &exe);
        return Err(format!("putting the new exe in place: {e}"));
    }

    let me = std::process::id();
    let event_name = wide(&healthy_event(cfg, me));
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, event_name.as_ptr()) };
    if event.is_null() {
        let _ = fs::rename(&exe, &new).and_then(|_| fs::rename(&old, &exe));
        return Err("no handover event".into());
    }
    (cfg.before_restart)();
    release();
    let mut args = vec![format!("--v15-handoff={me}")];
    args.extend((cfg.relaunch_args)());
    let child = Command::new(&exe).args(&args).spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            unsafe { CloseHandle(event) };
            let _ = fs::rename(&exe, &failed).and_then(|_| fs::rename(&old, &exe));
            acquire(cfg);
            (cfg.restart_aborted)();
            return Err(format!("starting build {}: {e}", c.build));
        }
    };
    log(format!("handing over to build {} (pid {})", c.build, child.id()));
    let shown = hide_windows(app);
    let waited = unsafe {
        let handles = [event, child.as_raw_handle() as HANDLE];
        WaitForMultipleObjects(2, handles.as_ptr(), 0, HANDOFF_TIMEOUT.as_millis() as u32)
    };
    unsafe { CloseHandle(event) };
    if waited == WAIT_OBJECT_0 {
        log(format!("build {} took over; exiting", c.build));
        std::process::exit(0);
    }

    // it exited, hung, or never loaded a page: back to this build
    let why = if waited == WAIT_OBJECT_0 + 1 {
        "exited"
    } else {
        "did not load a page in time"
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&failed);
    let restored = retry(|| fs::rename(&exe, &failed)).and_then(|_| retry(|| fs::rename(&old, &exe)));
    skip(cfg, c.build);
    acquire(cfg);
    show_windows(app, shown);
    (cfg.restart_aborted)();
    match restored {
        Ok(()) => Err(format!(
            "build {} {why}; kept build {} and will skip it",
            c.build, cfg.stamp.build
        )),
        Err(e) => Err(format!(
            "build {} {why}, and putting this build back failed: {e}",
            c.build
        )),
    }
}

fn hide_windows<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    app.webview_windows()
        .into_iter()
        .filter(|(_, w)| w.is_visible().unwrap_or(false))
        .map(|(label, w)| {
            let _ = w.hide();
            label
        })
        .collect()
}

fn show_windows<R: Runtime>(app: &AppHandle<R>, labels: Vec<String>) {
    for label in labels {
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.show();
        }
    }
}
