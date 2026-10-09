//! v15-shell: the portion of every v15 Tauri app that is not the app itself.
//!
//! Single portable executable, no installer: runs from any location, relocates itself
//! to its install folder, creates a Start-menu shortcut and an "Apps & features" entry,
//! then relaunches from there. Single instance: a second launch brings the first to the
//! foreground. Updates come from the app's manifest URL, Ed25519-signed builds only
//! (v15-update), downloaded behind a splash screen, swapped in, then handed over to:
//! the new build must load a page within a minute or the old one restores itself and
//! skips that build. docs/UPDATER.md.
//!
//! ```ignore
//! static SHELL: v15_shell::Config = v15_shell::Config {
//!     app: "myapp",
//!     name: "myapp",
//!     exe: "myapp.exe",
//!     identifier: "com.example.myapp",
//!     install: v15_shell::InstallDir::LocalPrograms("myapp"),
//!     manifest_url: "https://example.com/myapp/manifest.json",
//!     pubkeys: &["<hex from v15-release keygen>"],
//!     stamp: v15_shell::stamp!(),
//!     version: env!("CARGO_PKG_VERSION"),
//!     ..v15_shell::Config::DEFAULT
//! };
//!
//! pub fn run() {
//!     v15_shell::pre_gui(&SHELL); // may exit: installed elsewhere, or already running
//!     tauri::Builder::default()
//!         .plugin(v15_shell::plugin(&SHELL))
//!         .run(v15_shell::context(tauri::generate_context!()))
//!         .expect("error while running");
//! }
//! ```
//!
//! Debug builds, as well as any build executed with `V15_NO_BOOTSTRAP=1`, skip install,
//! single-instance, and updates. This behavior is currently Windows-only; on other
//! platforms, the plugin retains its commands but performs no other actions.
//!
//! A debug build is also the app's dev build ([`dev`]): when run through [`context`],
//! its identifier gains [`DEV_SUFFIX`], meaning its data, WebView2 profile, and log are
//! separate, and it runs alongside the installed copy.

mod commands;
pub mod defender;
mod log;
mod splash;
mod update;
#[cfg(windows)]
mod win;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tauri::plugin::TauriPlugin;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, Runtime};

pub use log::log;
pub use update::{check, Outcome};

/// The build embeds its build id (`V15_BUILD_ID`, epoch seconds, set by the
/// release workflow; 0 for local builds) and, for builds testing updates
/// against a local server only, `V15_TEST_MANIFEST_URL` and
/// `V15_TEST_PUBKEY` (a throwaway key such a build also accepts, ensuring
/// local tests never require the app's release key). Release workflows
/// reject both.
#[derive(Clone, Copy, Debug)]
pub struct Stamp {
    pub build: u64,
    pub test_manifest: Option<&'static str>,
    pub test_pubkey: Option<&'static str>,
}

/// Read in the app's own crate: cargo rebuilds a path crate when these
/// change, but not a git dependency like this one.
#[macro_export]
macro_rules! stamp {
    () => {{
        const STAMP: $crate::Stamp = $crate::Stamp {
            build: $crate::parse_build(option_env!("V15_BUILD_ID")),
            test_manifest: option_env!("V15_TEST_MANIFEST_URL"),
            test_pubkey: option_env!("V15_TEST_PUBKEY"),
        };
        STAMP
    }};
}

/// `V15_BUILD_ID` must consist entirely of digits; any other input
/// halts the build.
pub const fn parse_build(s: Option<&str>) -> u64 {
    let Some(s) = s else { return 0 };
    let b = s.as_bytes();
    assert!(!b.is_empty() && b.len() <= 19, "V15_BUILD_ID must be 1-19 digits");
    let mut n = 0u64;
    let mut i = 0;
    while i < b.len() {
        assert!(b[i].is_ascii_digit(), "V15_BUILD_ID must be digits");
        n = n * 10 + (b[i] - b'0') as u64;
        i += 1;
    }
    n
}

/// The installation location for the exe. It is per-user unless
/// `ProgramFiles`, which requires the app to run elevated (via its exe
/// manifest), otherwise a non-admin process could swap the files.
#[derive(Clone, Copy, Debug)]
pub enum InstallDir {
    /// `%LOCALAPPDATA%\Programs\<folder>`: default.
    LocalPrograms(&'static str),
    /// `%LOCALAPPDATA%\<folder>`
    LocalAppData(&'static str),
    /// `%APPDATA%\<folder>`
    RoamingAppData(&'static str),
    /// `%ProgramFiles%\<folder>`, machine-wide.
    ProgramFiles(&'static str),
}

impl InstallDir {
    pub fn machine(self) -> bool {
        matches!(self, InstallDir::ProgramFiles(_))
    }
}

/// The window in which an update downloads. Its page defines
/// `window.progress(fraction)`, which the download calls from 0 to 1
/// (resetting to 0 when a failed download is retried).
#[derive(Clone, Copy, Debug)]
pub enum Splash {
    /// The kit's own small page: the app's name, "Updating to `<version>`", a bar.
    Builtin,
    /// The app's own page, served from its frontend (such as one whose logo
    /// traces the download): it can use the app's scripts, fonts and images.
    /// The window's label is [`SPLASH_LABEL`], for capabilities.
    Page {
        path: &'static str,
        width: f64,
        height: f64,
    },
    /// A self-contained page; `{{name}}` and `{{version}}` are populated.
    Html {
        html: &'static str,
        width: f64,
        height: f64,
    },
    /// No window: the download happens out of sight.
    Off,
}

/// The splash window's label.
pub const SPLASH_LABEL: &str = splash::LABEL;

/// With `Config::forward_launch`, the event on which a second launch's
/// arguments reach the running copy: a `Vec<String>` (the program name is omitted).
pub const LAUNCH_EVENT: &str = "v15-shell://launch";

pub struct Config {
    /// The app's name in its manifest and signatures, and in the names of its
    /// mutex and events: `[a-z0-9-]`.
    pub app: &'static str,
    /// What people see: shortcut, "Apps & features", splash.
    pub name: &'static str,
    /// The file name of the installed exe.
    pub exe: &'static str,
    /// The Tauri identifier (its data folders are removed on
    /// uninstall).
    pub identifier: &'static str,
    pub install: InstallDir,
    pub manifest_url: &'static str,
    /// Hex Ed25519 keys whose signatures this app accepts (more than one
    /// only while rotating keys).
    pub pubkeys: &'static [&'static str],
    pub stamp: Stamp,
    /// Displayed, not compared: builds are sorted by build id.
    pub version: &'static str,
    pub publisher: &'static str,
    /// The window brought forward by a second launch.
    pub main_window: &'static str,
    pub desktop_shortcut: bool,
    /// The "Apps & features" entry's registry key; "" indicates `v15-<app>`.
    /// An app moving off an installer of its own keeps that installer's key,
    /// so the installs it already has aren't listed twice.
    pub uninstall_entry: &'static str,
    pub remove_data_on_uninstall: bool,
    /// Runs on uninstall after the running copy has quit, before anything
    /// is removed (e.g. stopping a helper process the app started).
    pub on_uninstall: fn(),
    /// Pass the arguments from a second launch to the running instance as
    /// [`LAUNCH_EVENT`] rather than merely bringing its main window to the
    /// front; the app then displays what it requires (a `myapp://` link, for
    /// example).
    pub forward_launch: bool,
    /// The webview data folder for apps whose windows set their own; the
    /// splash uses it too.
    pub webview_dir: Option<fn() -> PathBuf>,
    /// Folders that Microsoft Defender should leave alone (the app's data,
    /// its downloads). The app calls `defender::ensure(false)` after
    /// `pre_gui`: one UAC prompt for whatever is missing ([`defender`]).
    pub defender: Option<fn() -> Vec<PathBuf>>,
    pub first_check: Duration,
    pub check_every: Duration,
    /// The window behind which an update downloads ([`Splash`]).
    pub splash: Splash,
    /// Extra arguments for the build an update starts, such as
    /// "--reconnect".
    pub relaunch_args: fn() -> Vec<String>,
    /// Runs just before an update hands over to the new build...
    pub before_restart: fn(),
    /// ...and this if the new build did not take over and this one
    /// continues.
    pub restart_aborted: fn(),
}

fn no_args() -> Vec<String> {
    Vec::new()
}

fn nothing() {}

impl Config {
    pub const DEFAULT: Config = Config {
        app: "",
        name: "",
        exe: "",
        identifier: "",
        install: InstallDir::LocalPrograms(""),
        manifest_url: "",
        pubkeys: &[],
        stamp: Stamp {
            build: 0,
            test_manifest: None,
            test_pubkey: None,
        },
        version: "",
        publisher: "v15.studio",
        main_window: "main",
        desktop_shortcut: false,
        uninstall_entry: "",
        remove_data_on_uninstall: true,
        on_uninstall: nothing,
        forward_launch: false,
        webview_dir: None,
        defender: None,
        first_check: Duration::from_secs(4),
        check_every: Duration::from_secs(6 * 3600),
        splash: Splash::Builtin,
        relaunch_args: no_args,
        before_restart: nothing,
        restart_aborted: nothing,
    };

    fn validate(&self) {
        assert!(
            v15_update::valid_name(self.app) && !self.app.contains('_'),
            "Config.app must be [a-z0-9-]"
        );
        assert!(!self.name.is_empty() && self.exe.ends_with(".exe") && !self.identifier.is_empty());
        assert!(
            self.manifest_url.starts_with("https://"),
            "Config.manifest_url must be https"
        );
        assert!(
            !self.pubkeys.is_empty(),
            "Config.pubkeys: an app without a key can never update"
        );
        for k in self.pubkeys {
            v15_update::verifying_key(k).expect("Config.pubkeys holds a bad key");
        }
    }

    pub(crate) fn test_mode(&self) -> bool {
        self.stamp.test_manifest.is_some()
    }

    /// Builds stamped for update tests can be pointed at a scratch folder
    /// (`V15_TEST_ROOT`): they then create no shortcuts or registry entries,
    /// and write nothing outside it.
    pub(crate) fn test_root(&self) -> Option<PathBuf> {
        self.test_mode()
            .then(|| std::env::var_os("V15_TEST_ROOT").map(PathBuf::from))
            .flatten()
    }

    pub fn install_dir(&self) -> PathBuf {
        let folder = match self.install {
            InstallDir::LocalPrograms(f)
            | InstallDir::LocalAppData(f)
            | InstallDir::RoamingAppData(f)
            | InstallDir::ProgramFiles(f) => f,
        };
        if let Some(root) = self.test_root() {
            return root.join(folder);
        }
        let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let base = match self.install {
            InstallDir::LocalPrograms(_) => env("LOCALAPPDATA").map(|p| p.join("Programs")),
            InstallDir::LocalAppData(_) => env("LOCALAPPDATA"),
            InstallDir::RoamingAppData(_) => env("APPDATA"),
            InstallDir::ProgramFiles(_) => env("ProgramW6432").or_else(|| env("ProgramFiles")),
        };
        base.unwrap_or_else(std::env::temp_dir).join(folder)
    }

    /// Tauri's app_log_dir on Windows, usable before Tauri starts. A dev
    /// build's is its own (`<identifier>-dev`).
    pub fn log_dir(&self) -> PathBuf {
        if let Some(root) = self.test_root() {
            return root.join("logs");
        }
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(identifier_for(self.identifier, dev()))
            .join("logs")
    }
}

/// The suffix a dev build appends to the app's identifier, so its data,
/// config, WebView2 folders, and logs are stored apart from the installed
/// copy's.
pub const DEV_SUFFIX: &str = "-dev";

/// A dev build is any debug build (`cargo run`, the app's `<app>-dev`
/// launcher). It does not acquire a single-instance lock, does not
/// install anything, never updates, and maintains its own data via
/// [`context`].
pub fn dev() -> bool {
    cfg!(debug_assertions)
}

fn identifier_for(identifier: &str, dev: bool) -> String {
    if dev {
        format!("{identifier}{DEV_SUFFIX}")
    } else {
        identifier.to_string()
    }
}

/// The app's context: `.run(v15_shell::context(tauri::generate_context!()))`.
/// In a dev build, the identifier includes [`DEV_SUFFIX`] (causing every
/// folder Tauri derives from it to move), and the product name and configured
/// window titles include " dev". A release build's context is unchanged.
pub fn context<R: Runtime>(mut ctx: tauri::Context<R>) -> tauri::Context<R> {
    if dev() {
        dev_config(ctx.config_mut());
    }
    ctx
}

fn dev_config(c: &mut tauri::Config) {
    c.identifier = identifier_for(&c.identifier, true);
    if let Some(name) = &mut c.product_name {
        name.push_str(" dev");
    }
    for w in &mut c.app.windows {
        w.title.push_str(" dev");
    }
}

static CONFIG: OnceLock<&'static Config> = OnceLock::new();

pub(crate) fn config() -> &'static Config {
    CONFIG.get().copied().expect("v15_shell::pre_gui runs first")
}

/// Install, single instance, uninstall. Call first in `run()`: it may
/// exit the process.
pub fn pre_gui(cfg: &'static Config) {
    let _ = CONFIG.set(cfg);
    log::init(cfg);
    if !active() {
        return;
    }
    cfg.validate();
    #[cfg(windows)]
    win::pre_gui(cfg);
}

/// The bootstrap is enabled for release builds that have not opted out.
pub fn active() -> bool {
    !cfg!(debug_assertions) && std::env::var_os("V15_NO_BOOTSTRAP").is_none()
}

/// True in a build started by an update, until the build it replaces has
/// exited. Apps that terminate older instances of themselves
/// at startup must not do so during this time.
pub fn handing_over() -> bool {
    #[cfg(windows)]
    return win::handing_over();
    #[cfg(not(windows))]
    false
}

/// Whether this is the installed copy (only that copy updates).
pub fn installed() -> bool {
    let cfg = config();
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| same_dir(p, &cfg.install_dir())))
        .unwrap_or(false)
}

pub(crate) fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

/// Brings the main window to the front (second launch, tray "Show").
pub fn show<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window(config().main_window) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// The plugin: events from other processes, the handover check, the
/// update loop, and the `info` / `check` commands.
pub fn plugin<R: Runtime>(cfg: &'static Config) -> TauriPlugin<R> {
    let _ = CONFIG.set(cfg);
    tauri::plugin::Builder::new("v15-shell")
        .invoke_handler(tauri::generate_handler![commands::info, commands::check])
        .setup(move |app, _api| {
            if active() {
                #[cfg(windows)]
                win::post_gui(app, cfg);
                update::start_loop(app.clone(), cfg);
            }
            Ok(())
        })
        .on_page_load(|webview, payload| {
            if payload.event() == PageLoadEvent::Finished && webview.label() != splash::LABEL {
                #[cfg(windows)]
                win::mark_healthy();
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_ids_parse() {
        assert_eq!(parse_build(None), 0);
        assert_eq!(parse_build(Some("1791460800")), 1_791_460_800);
    }

    #[test]
    #[should_panic]
    fn a_bad_build_id_is_refused() {
        parse_build(Some("17914x"));
    }

    #[test]
    fn a_dev_build_keeps_its_own_data_and_titles() {
        let mut c: tauri::Config = serde_json::from_value(serde_json::json!({
            "identifier": "com.example.myapp",
            "productName": "My App",
            "app": { "windows": [{ "label": "main", "title": "My App" }] }
        }))
        .unwrap();
        dev_config(&mut c);
        assert_eq!(c.identifier, "com.example.myapp-dev");
        assert_eq!(c.product_name.as_deref(), Some("My App dev"));
        assert_eq!(c.app.windows[0].title, "My App dev");
    }

    #[test]
    fn a_dev_build_logs_beside_its_own_data() {
        assert_eq!(identifier_for("com.example.myapp", false), "com.example.myapp");
        assert_eq!(identifier_for("com.example.myapp", true), "com.example.myapp-dev");
        // tests are debug builds, so this config logs as a dev build
        let cfg = Config {
            identifier: "com.example.myapp",
            ..Config::DEFAULT
        };
        let dir = cfg.log_dir();
        assert!(dir.ends_with("com.example.myapp-dev/logs"), "{}", dir.display());
    }
}
