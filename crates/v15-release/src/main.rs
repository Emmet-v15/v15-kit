//! v15-release: the signing half of the v15 updater (docs/UPDATER.md).
//!
//!   v15-release keygen --out state/update-signing.pem
//!   v15-release pubkey --key state/update-signing.pem
//!   v15-release add --app A --platform windows-x86_64 --build B --version V \
//!       --file out/a.exe --url https://example.com/a/a.exe --br \
//!       --base https://example.com/a/manifest.json --out out/manifest.json \
//!       --expect-pubkey <hex>
//!   v15-release verify --manifest out/manifest.json --app A --platform P --pubkey <hex> [--file F]
//!   v15-release sums --out out/SHA256SUMS out/a.exe out/a.exe.br out/manifest.json
//!
//! The signing key is a PKCS#8 PEM Ed25519 key (what `openssl genpkey -algorithm ed25519` writes).
//! `add` reads it from --key, or from the V15_UPDATE_KEY environment variable (CI's secret), never
//! from a flag value.

use clap::{Parser, Subcommand};
use ed25519_dalek::pkcs8::{spki::der::pem::LineEnding, DecodePrivateKey, EncodePrivateKey};
use ed25519_dalek::{Signer, SigningKey};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use v15_update::{hex, sha256_hex, signed_message, valid_name, Artifact, Manifest, Packed};

#[derive(Parser)]
#[command(
    name = "v15-release",
    version,
    about = "Sign v15 app builds and write their update manifests"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate a new signing key (does not overwrite an existing one).
    Keygen {
        #[arg(long)]
        out: PathBuf,
    },
    /// Print the public half of a signing key, as apps compile it in
    Pubkey {
        #[arg(long)]
        key: Option<PathBuf>,
    },
    /// Sign a file from one platform and add it to a manifest
    Add {
        #[arg(long)]
        app: String,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        build: u64,
        #[arg(long)]
        version: String,
        /// The file the app runs (the exe, the apk)
        #[arg(long)]
        file: PathBuf,
        /// The location where the file is served
        #[arg(long)]
        url: String,
        /// Also write <file>.br and list it, to be served at <url>.br
        #[arg(long)]
        br: bool,
        #[arg(long, default_value = "")]
        notes: String,
        /// The live manifest to append to: a URL (404 = none yet) or a
        /// file
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        out: PathBuf,
        /// Signing key file; without it, the PEM in V15_UPDATE_KEY
        #[arg(long)]
        key: Option<PathBuf>,
        /// Refuse unless the key matches the app's compiled-in key
        #[arg(long)]
        expect_pubkey: Option<String>,
    },
    /// Show what an app would install from a manifest, and check a
    /// file against it
    Verify {
        #[arg(long)]
        manifest: String,
        #[arg(long)]
        app: String,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        pubkey: String,
        #[arg(long, default_value_t = 0)]
        current: u64,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Brotli-pack a file at quality 11, verifying it unpacks to identical
    /// bytes
    Brotli { input: PathBuf, output: PathBuf },
    /// Write SHA256SUMS (in sha256sum format) for release-pull
    Sums {
        #[arg(long)]
        out: PathBuf,
        files: Vec<PathBuf>,
    },
}

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    match run(Cli::parse().cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("v15-release: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Keygen { out } => keygen(&out),
        Cmd::Pubkey { key } => {
            println!("{}", hex(load_key(key.as_deref())?.verifying_key().as_bytes()));
            Ok(())
        }
        Cmd::Add {
            app,
            platform,
            build,
            version,
            file,
            url,
            br,
            notes,
            base,
            out,
            key,
            expect_pubkey,
        } => {
            let key = load_key(key.as_deref())?;
            let have = hex(key.verifying_key().as_bytes());
            if let Some(want) = expect_pubkey {
                if !want.eq_ignore_ascii_case(&have) {
                    return Err(format!(
                        "the signing key is {have}, the app trusts {want}: wrong secret?"
                    ));
                }
            }
            let mut manifest = match base {
                Some(b) => load_manifest(&b, true)?.unwrap_or_else(|| Manifest::new(&app)),
                None => Manifest::new(&app),
            };
            if manifest.app != app {
                return Err(format!("the base manifest is {:?}'s, not {app:?}'s", manifest.app));
            }
            let artifact = sign(&key, &app, &platform, build, &file, &url, br)?;
            eprintln!(
                "signed {app} {platform} build {build}: {} bytes, sha256 {}",
                artifact.size, artifact.sha256
            );
            manifest.add(build, &version, &now_rfc3339(), &notes, &platform, artifact);
            write(&out, manifest.to_json().as_bytes())?;
            eprintln!(
                "wrote {} (latest {}, {} builds)",
                out.display(),
                manifest.latest,
                manifest.builds.len()
            );
            Ok(())
        }
        Cmd::Verify {
            manifest,
            app,
            platform,
            pubkey,
            current,
            file,
        } => {
            let m = load_manifest(&manifest, false)?.ok_or("no manifest")?;
            let keys = [v15_update::verifying_key(&pubkey).map_err(|e| e.to_string())?];
            let c = v15_update::select(&m, &app, &platform, &keys, current, &[])
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    format!(
                        "no signed {platform} build above {current} (latest listed: {})",
                        m.latest
                    )
                })?;
            println!(
                "build {} ({}) {} bytes sha256 {}",
                c.build, c.version, c.artifact.size, c.artifact.sha256
            );
            if let Some(f) = file {
                let bytes = read(&f)?;
                if !v15_update::matches(&bytes, &c.artifact) {
                    return Err(format!("{} is not build {}'s file", f.display(), c.build));
                }
                println!("{} matches", f.display());
            }
            Ok(())
        }
        Cmd::Brotli { input, output } => {
            let packed = pack(&read(&input)?)?;
            write(&output, &packed)
        }
        Cmd::Sums { out, files } => {
            let mut text = String::new();
            for f in &files {
                let name = f
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| format!("bad file name {}", f.display()))?;
                text.push_str(&format!("{}  {name}\n", sha256_hex(&read(f)?)));
            }
            write(&out, text.as_bytes())
        }
    }
}

fn keygen(out: &Path) -> Result<()> {
    if out.exists() {
        return Err(format!(
            "{} exists: a lost or replaced key strands every installed app",
            out.display()
        ));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
    let key = SigningKey::from_bytes(&seed);
    let pem = key.to_pkcs8_pem(LineEnding::LF).map_err(|e| e.to_string())?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|e| e.to_string())?;
    f.write_all(pem.as_bytes()).map_err(|e| e.to_string())?;
    eprintln!(
        "wrote {} (keep it out of git; CI gets it as the V15_UPDATE_KEY secret)",
        out.display()
    );
    println!("{}", hex(key.verifying_key().as_bytes()));
    Ok(())
}

fn load_key(path: Option<&Path>) -> Result<SigningKey> {
    let pem = match path {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => std::env::var("V15_UPDATE_KEY").map_err(|_| "no --key and no V15_UPDATE_KEY".to_string())?,
    };
    SigningKey::from_pkcs8_pem(pem.trim()).map_err(|e| format!("not a PKCS#8 Ed25519 PEM key: {e}"))
}

fn sign(key: &SigningKey, app: &str, platform: &str, build: u64, file: &Path, url: &str, br: bool) -> Result<Artifact> {
    if !valid_name(app) || !valid_name(platform) {
        return Err(format!("app and platform must be [a-z0-9_-]: {app:?} {platform:?}"));
    }
    let bytes = read(file)?;
    let magic: &[u8] = match platform {
        p if p.starts_with("windows-") => b"MZ",
        p if p.starts_with("android-") => b"PK",
        _ => b"",
    };
    if !bytes.starts_with(magic) {
        return Err(format!("{} doesn't look like a {platform} build", file.display()));
    }
    let sha256 = sha256_hex(&bytes);
    let sig = key.sign(signed_message(app, platform, build, bytes.len() as u64, &sha256).as_bytes());
    let packed = if br {
        let out = PathBuf::from(format!("{}.br", file.display()));
        let p = pack(&bytes)?;
        write(&out, &p)?;
        eprintln!("wrote {} ({} -> {} bytes)", out.display(), bytes.len(), p.len());
        Some(Packed {
            url: format!("{url}.br"),
            size: p.len() as u64,
        })
    } else {
        None
    };
    let a = Artifact {
        url: url.to_string(),
        size: bytes.len() as u64,
        sha256,
        sig: hex(&sig.to_bytes()),
        br: packed,
    };
    // the app's own check before anything ships
    if !v15_update::verify(&[key.verifying_key()], app, platform, build, &a) {
        return Err("the signature doesn't verify (this is a bug)".into());
    }
    Ok(a)
}

fn pack(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let params = brotli::enc::BrotliEncoderParams {
            quality: 11,
            lgwin: 24,
            ..Default::default()
        };
        brotli::BrotliCompress(&mut &bytes[..], &mut out, &params).map_err(|e| e.to_string())?;
    }
    let mut back = Vec::with_capacity(bytes.len());
    brotli_decompressor::Decompressor::new(&out[..], 64 * 1024)
        .read_to_end(&mut back)
        .map_err(|e| e.to_string())?;
    if back != bytes {
        return Err("brotli did not round-trip".into());
    }
    Ok(out)
}

/// `missing_ok`: a URL returning a 404 means "no manifest yet" (the app's first
/// release).
fn load_manifest(src: &str, missing_ok: bool) -> Result<Option<Manifest>> {
    let text = if src.starts_with("https://") || src.starts_with("http://") {
        match ureq::get(src).timeout(Duration::from_secs(30)).call() {
            Ok(r) => r.into_string().map_err(|e| e.to_string())?,
            Err(ureq::Error::Status(404, _)) if missing_ok => return Ok(None),
            Err(e) => return Err(format!("{src}: {e}")),
        }
    } else {
        match std::fs::read_to_string(src) {
            Ok(t) => t,
            Err(e) if missing_ok && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{src}: {e}")),
        }
    };
    v15_update::parse(&text).map(Some).map_err(|e| format!("{src}: {e}"))
}

fn read(p: &Path) -> Result<Vec<u8>> {
    std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339(secs)
}

/// UTC, derived from epoch seconds (using Howard Hinnant's
/// civil_from_days).
fn rfc3339(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_791_460_800), "2026-10-08T12:00:00Z");
    }

    #[test]
    fn pem_keys_round_trip() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap();
        assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(SigningKey::from_pkcs8_pem(&pem).unwrap().to_bytes(), key.to_bytes());
    }

    #[test]
    fn signs_what_the_app_verifies_and_packs() {
        let dir = std::env::temp_dir().join(format!("v15-release-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("demo.exe");
        let body: Vec<u8> = b"MZ"
            .iter()
            .copied()
            .chain((0..50_000u32).map(|i| (i % 251) as u8))
            .collect();
        std::fs::write(&exe, &body).unwrap();
        let key = SigningKey::from_bytes(&[5; 32]);
        let a = sign(
            &key,
            "demo",
            v15_update::WINDOWS_X64,
            42,
            &exe,
            "https://x/demo.exe",
            true,
        )
        .unwrap();
        assert_eq!(a.br.as_ref().unwrap().url, "https://x/demo.exe.br");
        let mut m = Manifest::new("demo");
        m.add(42, "0.1.0", "d", "", v15_update::WINDOWS_X64, a);
        let m = v15_update::parse(&m.to_json()).unwrap();
        let c = v15_update::select(&m, "demo", v15_update::WINDOWS_X64, &[key.verifying_key()], 0, &[])
            .unwrap()
            .unwrap();
        let packed = std::fs::read(dir.join("demo.exe.br")).unwrap();
        let mut unpacked = Vec::new();
        brotli_decompressor::Decompressor::new(&packed[..], 4096)
            .read_to_end(&mut unpacked)
            .unwrap();
        assert!(v15_update::matches(&unpacked, &c.artifact));
        assert!(
            sign(&key, "demo", v15_update::ANDROID_ARM64, 1, &exe, "u", false).is_err(),
            "an exe isn't an apk"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
