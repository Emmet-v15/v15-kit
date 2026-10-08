use serde::Serialize;
use tauri::{AppHandle, Runtime};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Info {
    app: &'static str,
    version: &'static str,
    build: u64,
    installed: bool,
    updates: bool,
}

/// `invoke('plugin:v15-shell|info')`
#[tauri::command]
pub(crate) fn info() -> Info {
    let cfg = crate::config();
    Info {
        app: cfg.app,
        version: cfg.version,
        build: cfg.stamp.build,
        installed: crate::installed(),
        updates: crate::active() && crate::installed(),
    }
}

/// `invoke('plugin:v15-shell|check')`: "up-to-date", "busy",
/// "unsupported", or "failed: …". An update that installs restarts the app
/// instead.
#[tauri::command]
pub(crate) async fn check<R: Runtime>(app: AppHandle<R>) -> String {
    tauri::async_runtime::spawn_blocking(move || crate::check(&app).to_string())
        .await
        .unwrap_or_else(|e| format!("failed: {e}"))
}
