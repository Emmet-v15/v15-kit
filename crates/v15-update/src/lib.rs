//! The v15 update format is shared by the apps (which verify) and `v15-release` (which
//! signs). docs/UPDATER.md is the prose version.
//!
//! An app's manifest is located at a URL compiled into the app, for example
//! `https://example.com/<app>/manifest.json`:
//!
//! ```json
//! {
//!   "schema": 1,
//!   "app": "myapp",
//!   "latest": 1791234567,
//!   "builds": {
//!     "1791234567": {
//!       "version": "0.3.0",
//!       "date": "2026-10-08T12:00:00Z",
//!       "platforms": {
//!         "windows-x86_64": {
//!           "url": "https://example.com/myapp/myapp.exe",
//!           "size": 6123520,
//!           "sha256": "…",
//!           "sig": "…",
//!           "br": { "url": "https://example.com/myapp/myapp.exe.br", "size": 2101337 }
//!         }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! A build id is the release's epoch seconds: it only grows, and it is the Android
//! versionCode. An app sees a build only when its entry carries an Ed25519 signature,
//! under a key compiled into the app, over [`signed_message`]: the app, the platform,
//! the build, and the size and sha256 of the file it runs. Whoever controls the server
//! can withhold updates but cannot make an app run their bytes, move one app's build to
//! another app or platform, or roll an app back.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub use ed25519_dalek;

/// The manifest format this crate reads and writes.
pub const SCHEMA: u32 = 1;
/// How many builds a manifest keeps (the newest); older ones drop off.
pub const KEEP_BUILDS: usize = 5;

pub const WINDOWS_X64: &str = "windows-x86_64";
pub const LINUX_X64: &str = "linux-x86_64";
pub const ANDROID_ARM64: &str = "android-aarch64";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    pub schema: u32,
    pub app: String,
    pub latest: u64,
    pub builds: BTreeMap<u64, Build>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    pub platforms: BTreeMap<String, Artifact>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub url: String,
    /// Size and sha256 of the file that runs (for `br`, once unpacked).
    pub size: u64,
    pub sha256: String,
    /// Hex Ed25519 signature over [`signed_message`].
    pub sig: String,
    /// The same file brotli-packed: what desktop updaters download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub br: Option<Packed>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub url: String,
    pub size: u64,
}

/// A newer build that an app may install: signed, for this app and
/// platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub build: u64,
    pub version: String,
    pub artifact: Artifact,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not JSON, or not a manifest of schema [`SCHEMA`].
    Malformed(String),
    /// The manifest references a different app.
    WrongApp(String),
    /// An app or platform name outside `[a-z0-9_-]`, or an invalid
    /// key.
    Invalid(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Malformed(e) => write!(f, "malformed manifest: {e}"),
            Error::WrongApp(a) => write!(f, "the manifest is for {a:?}"),
            Error::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// App and platform names are included in the signed message line by line,
/// preventing them from containing newlines or other characters that could
/// forge additional lines.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The exact bytes a release signs and an app verifies. When the format
/// must change, alter the first line, not the structure.
pub fn signed_message(app: &str, platform: &str, build: u64, size: u64, sha256: &str) -> String {
    format!("v15-update/1\napp={app}\nplatform={platform}\nbuild={build}\nsize={size}\nsha256={sha256}\n")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A hex Ed25519 public key, as apps compile it in.
pub fn verifying_key(hex_key: &str) -> Result<VerifyingKey, Error> {
    let bytes: [u8; 32] = unhex(hex_key)
        .and_then(|k| k.try_into().ok())
        .ok_or_else(|| Error::Invalid(format!("not a 32-byte hex key: {hex_key:?}")))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| Error::Invalid(format!("bad public key: {e}")))
}

/// True if `sig` (hex) signs this artifact for this app, platform and
/// build under one of `keys`.
pub fn verify(keys: &[VerifyingKey], app: &str, platform: &str, build: u64, a: &Artifact) -> bool {
    let Some(sig) = unhex(&a.sig)
        .and_then(|s| <[u8; 64]>::try_from(s).ok())
        .map(|s| Signature::from_bytes(&s))
    else {
        return false;
    };
    if !valid_name(app) || !valid_name(platform) || unhex(&a.sha256).is_none_or(|h| h.len() != 32) {
        return false;
    }
    let msg = signed_message(app, platform, build, a.size, &a.sha256.to_ascii_lowercase());
    keys.iter().any(|k| k.verify_strict(msg.as_bytes(), &sig).is_ok())
}

/// Parses a manifest leniently: any unparseable build entry is dropped
/// (the caller logs this as "nothing newer"), ensuring a single bad
/// entry doesn't block all apps.
pub fn parse(json: &str) -> Result<Manifest, Error> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| Error::Malformed(e.to_string()))?;
    let schema = v["schema"]
        .as_u64()
        .ok_or_else(|| Error::Malformed("no schema".into()))?;
    if schema != SCHEMA as u64 {
        return Err(Error::Malformed(format!("schema {schema}, this build reads {SCHEMA}")));
    }
    let app = v["app"]
        .as_str()
        .ok_or_else(|| Error::Malformed("no app".into()))?
        .to_string();
    let latest = v["latest"].as_u64().unwrap_or(0);
    let mut builds = BTreeMap::new();
    if let Some(obj) = v["builds"].as_object() {
        for (id, b) in obj {
            if let (Ok(id), Ok(b)) = (id.parse::<u64>(), serde_json::from_value::<Build>(b.clone())) {
                builds.insert(id, b);
            }
        }
    }
    Ok(Manifest {
        schema: SCHEMA,
        app,
        latest,
        builds,
    })
}

/// The latest build above `current` signed for `app` on `platform`,
/// excluding `skip` (builds that previously failed to start here).
pub fn select(
    manifest: &Manifest,
    app: &str,
    platform: &str,
    keys: &[VerifyingKey],
    current: u64,
    skip: &[u64],
) -> Result<Option<Candidate>, Error> {
    if manifest.app != app {
        return Err(Error::WrongApp(manifest.app.clone()));
    }
    Ok(manifest
        .builds
        .iter()
        .rev()
        .filter(|(id, _)| **id > current && !skip.contains(id))
        .find_map(|(id, b)| {
            let a = b.platforms.get(platform)?;
            verify(keys, app, platform, *id, a).then(|| Candidate {
                build: *id,
                version: b.version.clone(),
                artifact: Artifact {
                    sha256: a.sha256.to_ascii_lowercase(),
                    ..a.clone()
                },
            })
        }))
}

/// True if the downloaded (and unpacked) bytes match the signed file.
pub fn matches(bytes: &[u8], a: &Artifact) -> bool {
    bytes.len() as u64 == a.size && sha256_hex(bytes) == a.sha256.to_ascii_lowercase()
}

impl Manifest {
    pub fn new(app: &str) -> Self {
        Manifest {
            schema: SCHEMA,
            app: app.to_string(),
            latest: 0,
            builds: BTreeMap::new(),
        }
    }

    /// Adds (or replaces) a single platform in a build, designates the
    /// latest build as `latest`, and retains the most recent
    /// [`KEEP_BUILDS`].
    pub fn add(&mut self, build: u64, version: &str, date: &str, notes: &str, platform: &str, artifact: Artifact) {
        let entry = self.builds.entry(build).or_insert_with(|| Build {
            version: version.to_string(),
            date: date.to_string(),
            notes: String::new(),
            platforms: BTreeMap::new(),
        });
        entry.version = version.to_string();
        entry.date = date.to_string();
        if !notes.is_empty() {
            entry.notes = notes.to_string();
        }
        entry.platforms.insert(platform.to_string(), artifact);
        while self.builds.len() > KEEP_BUILDS {
            let oldest = *self.builds.keys().next().expect("non-empty");
            self.builds.remove(&oldest);
        }
        self.latest = self.builds.keys().next_back().copied().unwrap_or(0);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a manifest always serializes") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // test keys, not release ones
    fn key(n: u8) -> SigningKey {
        SigningKey::from_bytes(&[n; 32])
    }

    fn artifact(k: &SigningKey, app: &str, platform: &str, build: u64, bytes: &[u8]) -> Artifact {
        let sha = sha256_hex(bytes);
        let sig = k.sign(signed_message(app, platform, build, bytes.len() as u64, &sha).as_bytes());
        Artifact {
            url: format!("u{build}"),
            size: bytes.len() as u64,
            sha256: sha,
            sig: hex(&sig.to_bytes()),
            br: None,
        }
    }

    fn manifest(entries: Vec<(u64, &str, Artifact)>) -> Manifest {
        let mut m = Manifest::new("demo");
        for (id, platform, a) in entries {
            m.add(id, "0.1.0", "2026-10-08T00:00:00Z", "", platform, a);
        }
        m
    }

    #[test]
    fn picks_the_newest_signed_build_above_current() {
        let k = key(7);
        let m = manifest(vec![
            (100, WINDOWS_X64, artifact(&k, "demo", WINDOWS_X64, 100, b"a")),
            (200, WINDOWS_X64, artifact(&k, "demo", WINDOWS_X64, 200, b"b")),
            (300, ANDROID_ARM64, artifact(&k, "demo", ANDROID_ARM64, 300, b"c")),
        ]);
        let keys = [k.verifying_key()];
        let c = select(&m, "demo", WINDOWS_X64, &keys, 150, &[]).unwrap().unwrap();
        assert_eq!((c.build, c.artifact.url.as_str()), (200, "u200"));
        assert_eq!(
            select(&m, "demo", WINDOWS_X64, &keys, 200, &[]).unwrap(),
            None,
            "never the same or older"
        );
        assert_eq!(
            select(&m, "demo", WINDOWS_X64, &keys, 150, &[200]).unwrap(),
            None,
            "a skipped build stays skipped"
        );
    }

    #[test]
    fn rejects_unsigned_forged_moved_and_foreign_entries() {
        let (k, other) = (key(7), key(8));
        let mut unsigned = artifact(&k, "demo", WINDOWS_X64, 400, b"d");
        unsigned.sig.clear();
        let mut swapped = artifact(&k, "demo", WINDOWS_X64, 500, b"e");
        swapped.sha256 = sha256_hex(b"evil"); // signature is for other bytes
        let m = manifest(vec![
            (100, WINDOWS_X64, artifact(&k, "demo", WINDOWS_X64, 100, b"a")),
            (200, WINDOWS_X64, artifact(&other, "demo", WINDOWS_X64, 200, b"b")), // unknown key
            (300, WINDOWS_X64, artifact(&k, "another-app", WINDOWS_X64, 300, b"c")), // another app's
            (400, WINDOWS_X64, unsigned),
            (500, WINDOWS_X64, swapped),
            (600, WINDOWS_X64, artifact(&k, "demo", WINDOWS_X64, 1, b"f")), // signed as build 1
        ]);
        // KEEP_BUILDS dropped build 100, and none of the rest passes
        assert_eq!(
            select(&m, "demo", WINDOWS_X64, &[k.verifying_key()], 0, &[]).unwrap(),
            None
        );
        let android = artifact(&k, "demo", ANDROID_ARM64, 700, b"g");
        let m = manifest(vec![(700, WINDOWS_X64, android)]); // android's signature on a windows slot
        assert_eq!(
            select(&m, "demo", WINDOWS_X64, &[k.verifying_key()], 0, &[]).unwrap(),
            None
        );
    }

    #[test]
    fn a_manifest_for_another_app_is_an_error() {
        let m = Manifest::new("other");
        assert_eq!(
            select(&m, "demo", WINDOWS_X64, &[], 0, &[]),
            Err(Error::WrongApp("other".into()))
        );
    }

    #[test]
    fn rotation_accepts_any_listed_key() {
        let (old, new) = (key(7), key(9));
        let m = manifest(vec![(100, WINDOWS_X64, artifact(&new, "demo", WINDOWS_X64, 100, b"a"))]);
        let keys = [old.verifying_key(), new.verifying_key()];
        assert!(select(&m, "demo", WINDOWS_X64, &keys, 0, &[]).unwrap().is_some());
    }

    #[test]
    fn add_keeps_the_newest_five_and_merges_platforms() {
        let k = key(7);
        let mut m = Manifest::new("demo");
        for id in 1..=7u64 {
            m.add(
                id,
                "0.1.0",
                "d",
                "",
                WINDOWS_X64,
                artifact(&k, "demo", WINDOWS_X64, id, b"x"),
            );
        }
        m.add(
            7,
            "0.1.0",
            "d",
            "notes",
            ANDROID_ARM64,
            artifact(&k, "demo", ANDROID_ARM64, 7, b"y"),
        );
        assert_eq!(m.builds.keys().copied().collect::<Vec<_>>(), vec![3, 4, 5, 6, 7]);
        assert_eq!(m.latest, 7);
        assert_eq!(m.builds[&7].platforms.len(), 2);
        assert_eq!(m.builds[&7].notes, "notes");
    }

    #[test]
    fn json_round_trips_and_bad_entries_are_dropped() {
        let k = key(7);
        let m = manifest(vec![(100, WINDOWS_X64, artifact(&k, "demo", WINDOWS_X64, 100, b"a"))]);
        let json = m.to_json();
        assert_eq!(parse(&json).unwrap(), m);
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v["builds"]["oops"] = serde_json::json!({"platforms": 5});
        v["builds"]["200"] = serde_json::json!({"version": 1});
        assert_eq!(parse(&v.to_string()).unwrap().builds.len(), 1);
        assert!(matches!(
            parse(r#"{"schema":2,"app":"demo"}"#),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn matches_checks_size_and_hash() {
        let a = artifact(&key(7), "demo", WINDOWS_X64, 1, b"hello");
        assert!(matches(b"hello", &a));
        assert!(!matches(b"hellO", &a));
        assert!(!matches(b"hello!", &a));
    }

    #[test]
    fn names_cannot_inject_lines() {
        assert!(valid_name("myapp") && valid_name(WINDOWS_X64) && valid_name(ANDROID_ARM64));
        assert!(!valid_name("demo\nbuild=9") && !valid_name("") && !valid_name("Demo"));
    }
}
