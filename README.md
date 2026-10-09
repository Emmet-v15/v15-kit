# v15-kit

A shared shell for Tauri 2 desktop apps on Windows: the app installs itself,
runs as a single instance, and self-updates from signed builds. Also includes
the tool for signing those builds and the CI workflows for releasing them.

| piece | what it is |
|---|---|
| [`crates/v15-shell`](crates/v15-shell) | the Tauri plugin an app adds: install, single instance, uninstall, update check, splash, handover and rollback |
| [`crates/v15-update`](crates/v15-update) | the update format: manifest, the signed message, verification, choosing a build |
| [`crates/v15-release`](crates/v15-release) | the signing CLI: `keygen`, `add` (sign a build into a manifest), `verify`, `sums` |
| [`ops/release-pull`](ops/release-pull) | installs a GitHub release's files on a server, from CI over a restricted SSH key |
| [`.github/workflows`](.github/workflows) | `tauri-ci.yml` and `tauri-release.yml`, called from each app's own workflows |
| [`docs/UPDATER.md`](docs/UPDATER.md) | how updates work: the manifest, signatures, keys, releasing, testing |
| [`docs/STANDARD.md`](docs/STANDARD.md) | the checklist an app on the kit follows |

## How an app uses it

```toml
# src-tauri/Cargo.toml
[dependencies]
tauri = { version = "=2.12.1", features = [] }
v15-shell = { git = "https://github.com/Emmet-v15/v15-kit", tag = "v0.5.1" }
```

```rust
static SHELL: v15_shell::Config = v15_shell::Config {
    app: "myapp",
    name: "My App",
    exe: "myapp.exe",
    identifier: "com.example.myapp",
    install: v15_shell::InstallDir::LocalPrograms("myapp"),
    manifest_url: "https://example.com/myapp/manifest.json",
    pubkeys: &["<hex public key from v15-release keygen>"],
    stamp: v15_shell::stamp!(),
    version: env!("CARGO_PKG_VERSION"),
    ..v15_shell::Config::DEFAULT
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    v15_shell::pre_gui(&SHELL); // may exit: installed elsewhere, or already running
    tauri::Builder::default()
        .plugin(v15_shell::plugin(&SHELL))
        // ...
        .run(tauri::generate_context!())
        .expect("error while running");
}
```

`capabilities/default.json` lists `v15-shell:default`, and a page can call
`invoke('plugin:v15-shell|info')` and `invoke('plugin:v15-shell|check')`.

Other `Config` options:

| option | does |
|---|---|
| `splash` | the update window: the kit's, the app's own page, or none |
| `forward_launch` | arguments from a second launch reach the running copy as `LAUNCH_EVENT` |
| `webview_dir` | the app's WebView2 folder, for the splash too |
| `defender` | folders to exclude from Defender; the app calls `defender::ensure` |
| `uninstall_entry` | retain the Apps & features entry from the older installer |
| `relaunch_args`, `before_restart`, `restart_aborted` | carry state across an update |
| `on_uninstall` | stop helpers before files go |

## Working on the kit

| | |
|---|---|
| check | `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked` |
| build the CLI | `cargo build --release -p v15-release` |
| try a change in an app | point the app at a local copy: `v15-shell = { path = "../v15-kit/crates/v15-shell" }` |
| release | tag `vX.Y.Z` on `main` and push the tag; an app moves when its `tag =` and its workflows' `@vX.Y.Z` / `kit-ref` do |

Any change affecting install or update requires the local end-to-end run in
[docs/UPDATER.md](docs/UPDATER.md#testing-an-update-locally): install from a
download folder, update, roll back a crashing and a hanging build, second
launch, and uninstall.

Rules that ensure installed apps continue to function:

- The signed message (`v15_update::signed_message`) and manifest schema form a
  protocol that installed apps verify against. To change them, add a new
  version line; do not edit the existing one.
- Tauri is pinned exactly (`tauri =2.12.1`, `tauri-build =2.7.1`,
  `tauri-plugin =2.7.1`), and apps pin the same versions; moving Tauri
  constitutes a kit release.
- Signing keys never go into a repo or a log. Apps store theirs in a
  gitignored `state/` folder and the `V15_UPDATE_KEY` Actions secret.
- rustfmt `max_width = 120`; clippy clean with `-D warnings`.
