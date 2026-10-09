# Building an app on v15-kit

What an app on the kit looks like, so every one of them can be worked on the
same way.

## Checklist

- [ ] **Toolchain**: `rust-toolchain.toml` copied from v15-kit (an exact
  stable version, `profile = "minimal"`).
- [ ] **Tauri pinned exactly** to the kit's: `tauri = "=2.12.1"`,
  `tauri-build = "=2.7.1"`. A mismatch fails to resolve, on purpose.
- [ ] **One Cargo workspace** per repo when it has more than one crate: one
  `Cargo.lock` at its root, the version in `[workspace.package]`, and none in
  `tauri.conf.json` or `package.json`.
- [ ] **lib.rs / main.rs split**: `main.rs` only calls `<app>_lib::run()`, and
  `run()` carries `#[cfg_attr(mobile, tauri::mobile_entry_point)]`.
- [ ] **v15-shell**: `pre_gui(&SHELL)` first in `run()`,
  `.plugin(plugin(&SHELL))` on the builder,
  `.run(v15_shell::context(tauri::generate_context!()))`, and no other
  self-install, single-instance or updater code.
- [ ] **A dev launcher**: `scripts/<app>-dev.cmd` and its Start Menu shortcut
  (below, "The dev launcher").
- [ ] **Signed updates**: a key from `v15-release keygen` in `state/`
  (gitignored), its public half in `Config.pubkeys`, the private half in the
  `V15_UPDATE_KEY` secret. See UPDATER.md.
- [ ] **Identifier** chosen for good: changing it moves the app's data
  folders, so migrate them or accept losing them.
- [ ] **A CSP** (never `null`) and a `capabilities/default.json` for the app's
  windows that lists `v15-shell:default` beside the app's own permissions.
- [ ] **No bundler or JS toolchain unless the frontend needs one**: a plain
  `ui/` folder with `withGlobalTauri` and a `custom-protocol` default feature
  builds with `cargo build` alone.
- [ ] **CI**: `.github/workflows/ci.yml` calls `tauri-ci.yml`, and a
  `release.yml` (workflow_dispatch) calls `tauri-release.yml`.

## UI

- **Custom, inline titlebar.** Windows are frameless (`decorations: false`)
  with no native titlebar, drawing their own bar that includes a drag region
  and window buttons within the app's design.
- **Text is never verbose.** Labels, buttons, and messages use the fewest
  words necessary to convey meaning.
- **The update splash is the app's own** (`Splash::Page`): its mark, sourced
  from the icon, is traced in lines clockwise to serve as the download's
  progress bar (UPDATER.md, "The splash").

## Naming

| Thing | Pattern |
|---|---|
| manifest | `<site>/manifest.json` |
| files | `<site>/<exe>` and `<exe>.br` |
| download link | `https://v15.studio/<app>.exe`: the same exe at the site root, the one link to give people (a second `to` for the exe in the release-pull config, before the manifest) |
| GitHub release | `<app>-<build id>` |
| release-pull config | `/etc/release-pull/<app>.json` on the server, kept in the app's repo |
| single instance | `Local\v15-<app>-instance` (`Global\` for machine-wide installs) |
| uninstall entry | `HKCU\…\Uninstall\v15-<app>` (`HKLM` for machine-wide installs; `Config.uninstall_entry` keeps an older name) |
| dev launcher | `scripts/<app>-dev.cmd`, and the Start Menu shortcut `<app>-dev` that runs it |
| dev build's data | `<identifier>-dev` (its data, config, WebView2 and log folders) |

## The dev launcher

Every app has a Start Menu entry for its dev build, `<app>-dev`, next to the
installed `<app>`: one click builds the debug app from its repo and runs it,
starting the app's dev server first when it has one (star-dev's shape).

1. Copy v15-kit's `scripts/app-dev.cmd` to `scripts/<app>-dev.cmd` in the
   app's root (the folder holding `src-tauri/` or `tauri.conf.json`): set
   `<app>`, the run line (`cargo run -p <package>`), and the dev-server line
   or delete it.
2. Make the shortcut, and again whenever the repo moves:
   `powershell -NoProfile -File <v15-kit>\scripts\dev-shortcut.ps1 -Launcher scripts\<app>-dev.cmd`.
   It is per user (`%APPDATA%\Microsoft\Windows\Start Menu\Programs`), named
   after the launcher, and wears the app's `icon.ico`; `-Remove` takes it away.
3. Add the launcher to the `dev` row of the app's AGENTS.md.

The dev build runs beside the installed copy:

| | installed (release) | dev (debug) |
|---|---|---|
| single instance | `Local\v15-<app>-instance` | none: it never takes the lock, so it neither blocks nor is blocked |
| install, shortcuts, updates | yes | none |
| data, config, WebView2, log | `%APPDATA%\<identifier>`, `%LOCALAPPDATA%\<identifier>` | the same with `<identifier>-dev`, through `v15_shell::context` |
| product name, configured window titles | `<name>` | `<name> dev` |

What an app keeps outside Tauri's folders (a hard-coded path, a registry key,
a URL scheme, a fixed pipe name) is still shared: the app suffixes those
itself when `v15_shell::dev()` is true.

## Not in the kit yet

Tray (show / quit / hide on close), JSON settings, Linux and Android updates,
a "check now" push trigger, URL protocol registration, and Windows Defender
exclusions. Each is added when the first app requiring it moves to the kit.
