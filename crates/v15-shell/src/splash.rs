//! The window an update downloads behind ([`crate::Splash`]) is one of three:
//! the kit's own page (a data: URL, requiring no app frontend), the app's own
//! page from its frontend (including its scripts and images), or HTML
//! provided by the app. Each defines `window.progress(f)`, which the download
//! drives.

use crate::{Config, Splash};
use std::time::Duration;
use tauri::webview::PageLoadEvent;
use tauri::window::Color;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub(crate) const LABEL: &str = "v15-splash";

/// Shown under the page until it renders. WebView2 defaults to a white background,
/// and since a new webview often takes longer to paint than a small update takes to download,
/// the splash screen displayed only white.
const BACKGROUND: Color = Color(13, 15, 18, 255);
/// How long to wait for the page before showing the window and starting the download anyway.
const LOAD_WAIT: Duration = Duration::from_secs(3);

const PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:#0d0f12;color:#e8eaed;font:14px/1.4 system-ui,"Segoe UI",sans-serif;
  display:flex;align-items:center;justify-content:center;user-select:none;cursor:default}
main{width:236px}
h1{font-size:15px;font-weight:600;margin:0 0 2px}
p{margin:0 0 18px;color:#8b9099;font-size:12px}
.bar{height:4px;border-radius:2px;background:#22262c;overflow:hidden}
.fill{height:100%;width:0;background:#e8eaed;transition:width .2s}
</style></head><body><main>
<h1>{{name}}</h1><p>Updating to {{version}}</p>
<div class="bar"><div class="fill" id="f"></div></div>
</main><script>
window.progress=function(f){document.getElementById('f').style.width=(Math.max(0,Math.min(1,f))*100)+'%'};
</script></body></html>"#;

fn data_url(html: &str) -> String {
    let mut s = String::from("data:text/html;charset=utf-8,");
    for b in html.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// What to load and its size, or None for `Splash::Off`.
fn page(cfg: &Config, version: &str) -> Option<(WebviewUrl, f64, f64)> {
    let fill = |html: &str| {
        html.replace("{{name}}", &escape(cfg.name))
            .replace("{{version}}", &escape(version))
    };
    let external = |html: String| data_url(&html).parse().ok().map(WebviewUrl::External);
    match cfg.splash {
        Splash::Off => None,
        Splash::Builtin => external(fill(PAGE)).map(|u| (u, 300.0, 120.0)),
        Splash::Html { html, width, height } => external(fill(html)).map(|u| (u, width, height)),
        Splash::Page { path, width, height } => Some((WebviewUrl::App(path.into()), width, height)),
    }
}

pub(crate) fn show<R: Runtime>(app: &AppHandle<R>, cfg: &Config, version: &str) {
    let Some((url, width, height)) = page(cfg, version) else {
        return;
    };
    // WebView2 executes all webviews within a profile in a single browser
    // process. Consequently, a second window must request the exact browser
    // arguments used by the main window; if any other arguments are
    // provided, the window will not be created.
    #[cfg(windows)]
    let args = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == cfg.main_window)
        .and_then(|w| w.additional_browser_args.clone());
    let handle = app.clone();
    let title = cfg.name.to_string();
    let webview_dir = cfg.webview_dir;
    let (tx, rx) = std::sync::mpsc::channel();
    let (loaded_tx, loaded_rx) = std::sync::mpsc::channel::<()>();
    let _ = app.run_on_main_thread(move || {
        // hidden until the page loads, then shown painted: never a white window
        let loaded_tx = std::sync::Mutex::new(Some(loaded_tx));
        let mut builder = WebviewWindowBuilder::new(&handle, LABEL, url)
            .visible(false)
            .background_color(BACKGROUND)
            .on_page_load(move |w, payload| {
                if payload.event() == PageLoadEvent::Finished {
                    let _ = w.show();
                    if let Some(tx) = loaded_tx.lock().ok().and_then(|mut t| t.take()) {
                        let _ = tx.send(());
                    }
                }
            });
        if let Some(dir) = webview_dir {
            builder = builder.data_directory(dir());
        }
        #[cfg(windows)]
        if let Some(args) = &args {
            builder = builder.additional_browser_args(args);
        }
        let built = builder
            .title(title)
            .theme(Some(tauri::Theme::Dark))
            .inner_size(width, height)
            .resizable(false)
            .maximizable(false)
            .minimizable(false)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .center()
            .build();
        let _ = tx.send(built.map(|_| ()).map_err(|e| e.to_string()));
    });
    match rx.recv_timeout(Duration::from_secs(10)) {
        // The loaded page has defined progress(); a page that never loads still gets its window
        Ok(Ok(())) => {
            if loaded_rx.recv_timeout(LOAD_WAIT).is_err() {
                if let Some(w) = app.get_webview_window(LABEL) {
                    let _ = w.show();
                }
            }
        }
        Ok(Err(e)) => crate::log(format!("no splash: {e}")),
        Err(_) => crate::log("no splash: timed out"),
    }
}

pub(crate) fn progress<R: Runtime>(app: &AppHandle<R>, fraction: f64) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.eval(format!(
            "window.progress&&window.progress({})",
            fraction.clamp(0.0, 1.0)
        ));
    }
}

pub(crate) fn close<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_escape_everything_but_plain_text() {
        let u = data_url("<p a=\"b\">x y+z%</p>");
        assert_eq!(
            u,
            "data:text/html;charset=utf-8,%3Cp%20a%3D%22b%22%3Ex%20y%2Bz%25%3C%2Fp%3E"
        );
    }

    #[test]
    fn each_kind_of_splash() {
        let cfg = |splash| Config {
            name: "a<b",
            splash,
            ..Config::DEFAULT
        };
        assert!(page(&cfg(Splash::Off), "1").is_none());
        let (url, w, h) = page(
            &cfg(Splash::Page {
                path: "splash.html",
                width: 280.0,
                height: 230.0,
            }),
            "1",
        )
        .unwrap();
        assert!(matches!(url, WebviewUrl::App(p) if p.to_str() == Some("splash.html")));
        assert_eq!((w, h), (280.0, 230.0));
        let (url, ..) = page(
            &cfg(Splash::Html {
                html: "{{name}} {{version}}",
                width: 1.0,
                height: 1.0,
            }),
            "2.0",
        )
        .unwrap();
        assert!(matches!(url, WebviewUrl::External(u) if u.as_str().ends_with(&data_url("a&lt;b 2.0")[29..])));
        assert!(matches!(
            page(&cfg(Splash::Builtin), "1"),
            Some((WebviewUrl::External(_), 300.0, 120.0))
        ));
    }
}
