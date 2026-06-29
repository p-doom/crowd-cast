//! Offline release tool: Ed25519-sign a Linux appcast manifest.
//!
//! Produces the detached `.sig` that the in-app updater (`ui::updater_linux`) verifies. It signs
//! the SAME domain-separated message the verifier checks (`appcast_sig::signing_message`), using
//! `ed25519-dalek` so the signature is byte-compatible with the verifier (no openssl/libsodium
//! format drift). The signing key is the shared cross-platform Ed25519 key (the one Sparkle /
//! WinSparkle use); this tool accepts it either as the raw 32-byte seed OR as the 64-byte
//! libsodium secret key (`seed || pubkey`, as Sparkle exports it) and slices the seed out.
//!
//! Build/run (kept out of normal app builds behind a feature):
//!   cargo build --release --features release-tools --bin cc-sign-manifest
//!   CROWD_CAST_ED_PRIVATE_KEY=<base64> \
//!     ./cc-sign-manifest --manifest dist/appcast-linux.json --out dist/appcast-linux.json.sig
//!
//! It also prints the derived public key (base64) to stderr so the release can confirm it matches
//! the `CROWD_CAST_UPDATE_PUBKEY` baked into the binary.

// Shared with the in-app verifier so the signed-message construction can never drift.
#[path = "../ui/appcast_sig.rs"]
mod appcast_sig;

use std::process::ExitCode;

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer, SigningKey};

fn usage() -> String {
    "usage: cc-sign-manifest --manifest <path> [--out <path>] [--key-file <path>]\n\
     \n\
     The private key is read from --key-file or the CROWD_CAST_ED_PRIVATE_KEY env var (base64;\n\
     either a 32-byte seed or a 64-byte seed||pubkey). --out defaults to <manifest>.sig."
        .to_string()
}

fn run() -> Result<(), String> {
    let mut manifest: Option<String> = None;
    let mut out: Option<String> = None;
    let mut key_file: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" => manifest = Some(args.next().ok_or("--manifest needs a value")?),
            "--out" => out = Some(args.next().ok_or("--out needs a value")?),
            "--key-file" => key_file = Some(args.next().ok_or("--key-file needs a value")?),
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
        }
    }

    let manifest_path = manifest.ok_or_else(|| format!("missing --manifest\n\n{}", usage()))?;
    let out_path = out.unwrap_or_else(|| format!("{manifest_path}.sig"));

    // Key: --key-file wins, else the env var. Never echo it.
    let key_b64 = match key_file {
        Some(path) => std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read key file {path}: {e}"))?,
        None => std::env::var("CROWD_CAST_ED_PRIVATE_KEY")
            .map_err(|_| "no --key-file and CROWD_CAST_ED_PRIVATE_KEY is unset".to_string())?,
    };
    let key_bytes = STANDARD
        .decode(key_b64.trim())
        .map_err(|e| format!("private key is not valid base64: {e}"))?;

    // Accept the raw 32-byte seed or the 64-byte libsodium secret (seed||pubkey).
    let seed: [u8; 32] = match key_bytes.len() {
        32 => key_bytes[..32].try_into().unwrap(),
        64 => key_bytes[..32].try_into().unwrap(),
        n => {
            return Err(format!(
                "private key must decode to 32 bytes (seed) or 64 bytes (seed||pubkey), got {n}"
            ))
        }
    };
    let signing_key = SigningKey::from_bytes(&seed);

    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| format!("failed to read manifest {manifest_path}: {e}"))?;

    // Sign the domain-separated message — identical to what the verifier reconstructs.
    let signature = signing_key.sign(&appcast_sig::signing_message(&manifest_bytes));
    let sig_b64 = STANDARD.encode(signature.to_bytes());

    std::fs::write(&out_path, &sig_b64)
        .map_err(|e| format!("failed to write signature {out_path}: {e}"))?;

    let pubkey_b64 = STANDARD.encode(signing_key.verifying_key().to_bytes());
    eprintln!("signed {manifest_path} -> {out_path}");
    eprintln!("public key (must equal the baked CROWD_CAST_UPDATE_PUBKEY): {pubkey_b64}");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
