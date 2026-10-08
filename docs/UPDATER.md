# The v15 updater

One portable exe per app, no installer, with updates that are always signed
(`crates/v15-shell`, `crates/v15-update`, `crates/v15-release`). It doesn't
use `tauri-plugin-updater` or an NSIS installer; the exe installs and replaces
itself.

## What a user sees

1. They download `<app>.exe` and execute it. It relocates itself to its
   install folder (`%LOCALAPPDATA%\Programs\<app>` by default), creates a
   Start-menu shortcut and an "Apps & features" entry, then relaunches from
   that location.
2. Several seconds after each start, and every 6 hours, it reads the app's
   manifest. A newer signed build is downloaded behind a small splash window,
   replaced, and started; the old version exits after the new one loads a
   page.
3. If the new build crashes or fails to load a page within a minute, the old
   version restores itself, marks that build as bad, and continues. A fix
   arrives as a newer build.

## The splash

The app styles the window behind which the download occurs (`Config.splash`):

| `Splash::` | what shows |
|---|---|
| `Builtin` (default) | the kit's small page: name, "Updating to <version>", a bar |
| `Page { path, width, height }` | the app's own page from its frontend, with its scripts, fonts and images |
| `Html { html, width, height }` | a self-contained page; `{{name}}` and `{{version}}` are filled in |
| `Off` | nothing; the download happens out of sight |

The page defines `window.progress(fraction)`; the download calls it from 0 to
1, then back to 0 when a failed download is retried. The window's label is
`v15_shell::SPLASH_LABEL` (`v15-splash`), for any capability the page needs
(e.g. dragging). It opens with the main window's `additionalBrowserArgs`,
because WebView2 won't create a second window whose arguments differ.

## The manifest

```json
{
  "schema": 1,
  "app": "myapp",
  "latest": 1791460800,
  "builds": {
    "1791460800": {
      "version": "0.2.0",
      "date": "2026-10-08T12:00:00Z",
      "platforms": {
        "windows-x86_64": {
          "url": "https://example.com/myapp/myapp.exe",
          "size": 10817536,
          "sha256": "…",
          "sig": "…",
          "br": { "url": "https://example.com/myapp/myapp.exe.br", "size": 2616079 }
        }
      }
    }
  }
}
```

- **Build id** = the release's epoch seconds (`V15_BUILD_ID`, stamped by the
  release workflow). Builds are ordered by this value, never by `version`,
  which is for display only. It also serves as an Android versionCode (until
  2038).
- The newest 5 builds remain listed. Files are served at fixed URLs, so only
  the latest is downloadable; the history is for human reference.
- Platforms: `windows-x86_64` today; `linux-x86_64` and `android-aarch64` are
  reserved in the same format.

## The signature

Each platform entry includes a hex Ed25519 signature over exactly:

```
v15-update/1
app=<app>
platform=<platform>
build=<build id>
size=<bytes of the file that runs>
sha256=<its hex sha256>
```

An app only accepts an entry if its signature verifies against a key compiled
into it (`Config.pubkeys`), the build is newer than the current one, and the
download (unpacked, for `br`) matches the expected size and sha256. Whoever
controls the server can withhold updates, but cannot ship their own bytes, move
an app's build to a different app or platform, or roll an app back.

## Keys

- One key per app: `v15-release keygen --out state/update-signing.pem` in the
  app's repo (`state/` is gitignored). It prints the public key for
  `Config.pubkeys`.
- The private key exists in only two locations: that file, and the app repo's
  `V15_UPDATE_KEY` Actions secret (bash: `gh secret set V15_UPDATE_KEY <
  state/update-signing.pem`; PowerShell: `Get-Content state\update-signing.pem
  -Raw | gh secret set V15_UPDATE_KEY`).
- Losing it leaves every installed copy stuck on its current build. To rotate,
  ship a build whose `pubkeys` lists the old and new keys, then sign with the
  new one.
- The release workflow rejects any key the app doesn't trust
  (`--expect-pubkey`).
- Keys use PKCS#8 PEM, the format `openssl genpkey -algorithm ed25519` writes,
  so an existing OpenSSL Ed25519 key works as is.

## Releasing

`tauri-release.yml` (reusable): build with `V15_BUILD_ID` → `v15-release add`
signs the exe into the live manifest plus this build and packs the `.br` →
`v15-release verify` → `SHA256SUMS` → GitHub release `<app>-<build>` (newest 5
kept) → the server's `release-pull <app> <tag>` installs the files where the
app's config says (`/etc/release-pull/<app>.json`, installed with
`ops/release-pull/install.sh`) → the live manifest is checked.

Reinstalling an older release only affects what new downloads receive;
installed apps never roll back.

## Moving an existing app onto the kit

An app with its own updater requires one **bridge release**: published via the
old updater and old manifest, it is the first build on the kit. After that,
every update is signed. If the old updater did not verify signatures, that
step is no safer than before. Continue writing the old manifest until no old
installs remain.

The install folder (`InstallDir`) must match the old updater's location, or
the bridge build installs a second copy. `Config.uninstall_entry` keeps the
old "Apps & features" entry.

## Testing an update locally

Builds stamped with `V15_TEST_MANIFEST_URL` read that manifest instead,
and respect `V15_TEST_ROOT` (install folder and log go there; no shortcuts, no
registry).
With `V15_TEST_PUBKEY` they also accept a throwaway key, so a test never needs
the app's release key. Release workflows refuse to build with either set.

```sh
v15-release keygen --out <scratch>/test-key.pem    # prints the test pubkey
export V15_TEST_MANIFEST_URL=http://127.0.0.1:8765/manifest.json V15_TEST_PUBKEY=<that pubkey>
V15_BUILD_ID=100 cargo build --release -p <app>   # copy out as A
V15_BUILD_ID=200 cargo build --release -p <app>   # publish as B:
v15-release add --app <app> --platform windows-x86_64 --build 200 --version x \
  --file <exe> --url http://127.0.0.1:8765/<exe> --br --out manifest.json --key <scratch>/test-key.pem
python -m http.server 8765 --bind 127.0.0.1
V15_TEST_ROOT=<scratch> ./A.exe                   # installs, updates to 200
```

When an app shares its identifier with an installed copy on the same machine,
build the test using a different identifier
(`TAURI_CONFIG='{"identifier":"..."}'`) to prevent the test from accessing the
real copy's data.

The shell's log is `<app_log_dir>/v15-shell.log`
(`%LOCALAPPDATA%\<identifier>\logs`, or `<V15_TEST_ROOT>\logs`).
