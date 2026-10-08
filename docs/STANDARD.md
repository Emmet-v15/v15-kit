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
  `.plugin(plugin(&SHELL))` on the builder, and no other self-install,
  single-instance or updater code.
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
| GitHub release | `<app>-<build id>` |
| release-pull config | `/etc/release-pull/<app>.json` on the server, kept in the app's repo |
| single instance | `Local\v15-<app>-instance` (`Global\` for machine-wide installs) |
| uninstall entry | `HKCU\…\Uninstall\v15-<app>` (`HKLM` for machine-wide installs; `Config.uninstall_entry` keeps an older name) |

## Not in the kit yet

Tray (show / quit / hide on close), JSON settings, Linux and Android updates,
a "check now" push trigger, URL protocol registration, and Windows Defender
exclusions. Each is added when the first app requiring it moves to the kit.
