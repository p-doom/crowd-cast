//! Linux auto-update backend.
//!
//! Behavioral mirror of the macOS Sparkle codepath (`updater.rs` + `updater_darwin.m`),
//! built from the pieces Sparkle gives us for free on macOS:
//!
//! * **Feed**: one Ed25519-signed JSON manifest (vs. Sparkle's signed appcast). The
//!   manifest covers BOTH artifacts — the small per-release binary and the rarely-changing
//!   relocatable libobs bundle. Because the whole
//!   payload is small (~5 MB binary + ~17 MB bundle), we do NOT model separate "binary-only"
//!   vs "bundle-wide" update channels: it's one atomic versioned release, and we simply fetch
//!   whichever artifact's hash differs from what's installed (so "binary-only" falls out of a
//!   hash compare, and the binary↔bundle ABI match holds by construction).
//! * **Trust**: the manifest is signed with raw Ed25519 — the *same algorithm and key* as the
//!   macOS `SUPublicEDKey` / Sparkle `sign_update`. The 32-byte public key is baked in at build
//!   time (`CROWD_CAST_UPDATE_PUBKEY`); the per-artifact SHA-256 inside the signed manifest then
//!   authenticates each download (verify-before-swap).
//! * **Apply**: replace the running binary in place (rename-over is legal on Linux) and re-exec —
//!   reusing the same clean-stop handshake as macOS (`EngineCommand::PrepareForUpdate`) so we
//!   never yank the floor out from under an in-progress capture.
//!
//! **Driven by the tray**: this `LinuxUpdater` is wrapped by `UpdaterController` (updater.rs) and
//! driven by the shared tray loop (tray.rs) exactly as Sparkle is on macOS — periodic
//! `check_for_updates_in_background`, `set_busy(status_blocks_immediate_update)`, and the
//! `take_prepare_for_update_request` → `PrepareForUpdate` quiesce handshake. Background checks run
//! on a worker thread so the synchronous tray loop never blocks on a download.
//!
//! **Inert unless configured**: if `CROWD_CAST_UPDATE_FEED_URL` / `CROWD_CAST_UPDATE_PUBKEY`
//! weren't set at build time, `is_available()` is false and the controller is a no-op — so this is
//! a no-op for builds that don't opt in.
//!
//! NOTE: a changed libobs bundle (a rare OBS-ABI bump) is extracted into its versioned dir
//! (`~/.local/share/crowd-cast/obs/<abi>/`); the re-exec'd binary then activates it on startup via
//! the self-provisioning path (RUNPATH + StartupPaths in src/capture/context.rs). The common case —
//! a binary-only bugfix at the same ABI — applies end-to-end.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

/// Baked at build time by `build.rs`. Absent => updater unavailable (mirrors macOS treating a
/// missing `SUFeedURL` as "auto-update unavailable").
const FEED_URL: Option<&str> = option_env!("CROWD_CAST_UPDATE_FEED_URL");
/// Base64 of the 32-byte raw Ed25519 public key — the same key material as `SUPublicEDKey`.
const PUBKEY_B64: Option<&str> = option_env!("CROWD_CAST_UPDATE_PUBKEY");

/// The release version this binary was built as.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Monotonic build number baked by `build.rs` (the release workflow passes `github.run_number`;
/// dev builds get `"0"`). Combined with `CURRENT_VERSION` it forms the comparable version the
/// updater uses to decide "is the manifest strictly newer" — the analog of Sparkle's build number.
fn current_build() -> u64 {
    option_env!("CROWD_CAST_BUILD_NUMBER")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Manifest (the signed feed)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Release marketing version == the binary's `CARGO_PKG_VERSION`.
    pub version: String,
    /// Monotonic build number for this release (the workflow's `github.run_number`). Lets two
    /// releases share a marketing `version` and still be ordered; compared as a tiebreak by
    /// `is_newer`. Defaults to 0 for older feeds that predate the field.
    #[serde(default)]
    pub build: u64,
    #[serde(default)]
    pub notes: String,
    /// Forward-compat: marks this release as critical. Parsed today (so current clients accept
    /// future feeds that set it) but not yet acted on — the updater stays silent-by-default to
    /// respect the `PrepareForUpdate` quiesce contract. Enforcement is a later toggle.
    #[serde(default)]
    pub critical: bool,
    /// Forward-compat: the minimum version that may skip this update. Same parse-now/act-later
    /// treatment as `critical`.
    #[serde(default)]
    pub minimum_version: String,
    pub binary: BinaryArtifact,
    pub bundle: BundleArtifact,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinaryArtifact {
    pub url: String,
    /// Lowercase hex SHA-256 of the artifact.
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleArtifact {
    /// libobs/OBS ABI this release was built against (e.g. "32.0.2"). Used as the install dir.
    pub abi: String,
    pub url: String,
    pub sha256: String,
}

/// Persisted record of what's currently installed, used for the "fetch what changed" decision.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InstalledState {
    #[serde(default)]
    binary_version: String,
    #[serde(default)]
    bundle_abi: String,
    #[serde(default)]
    bundle_sha256: String,
}

/// What `check_and_stage` decided to fetch, downloaded + verified and ready to `apply`.
#[derive(Debug)]
struct StagedUpdate {
    version: String,
    /// Verified new binary in the work dir, if the binary changed.
    new_binary: Option<PathBuf>,
    /// (abi, sha256, verified archive in work dir), if the bundle changed.
    new_bundle: Option<(String, String, PathBuf)>,
}

/// Which artifacts differ from what's installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Plan {
    binary_changed: bool,
    bundle_changed: bool,
}

impl Plan {
    fn nothing(&self) -> bool {
        !self.binary_changed && !self.bundle_changed
    }
}

/// Parse a `major.minor.patch` string into a comparable tuple, ignoring any pre-release/build
/// suffix and tolerating missing components (treated as 0). Lenient by design: a feed should
/// never fail to apply because of an unusual-but-ordered version string.
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut it = v.split('.').map(|part| {
        // Take the leading digit run so "1.0.4-rc1" / "1.0.4+meta" parse as 1.0.4.
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<u64>().unwrap_or(0)
    });
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Whether `(new_version, new_build)` is strictly newer than `(cur_version, cur_build)`. Version
/// dominates; build number is the tiebreak within the same version. This replaces a bare string
/// inequality so (a) a same-version rebuild can still ship and (b) a feed that points at an OLDER
/// release never triggers a downgrade.
fn is_newer(cur_version: &str, cur_build: u64, new_version: &str, new_build: u64) -> bool {
    let (cur, new) = (parse_version(cur_version), parse_version(new_version));
    (new, new_build) > (cur, cur_build)
}

/// Compare a manifest against what's installed. Pure (unit-tested): the binary updates only when
/// the manifest is strictly newer; "binary-only" is just the case where the bundle hash matches.
fn plan_update(
    manifest: &Manifest,
    current_version: &str,
    current_build: u64,
    installed: &InstalledState,
) -> Plan {
    Plan {
        binary_changed: is_newer(current_version, current_build, &manifest.version, manifest.build),
        bundle_changed: manifest.bundle.sha256 != installed.bundle_sha256,
    }
}

// ---------------------------------------------------------------------------
// Crypto / hashing (pure, unit-tested)
// ---------------------------------------------------------------------------

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_lower(&h.finalize())
}

/// Verify a detached raw-Ed25519 signature (base64) over `message` against a base64 32-byte
/// public key. Same scheme as Sparkle's `sign_update` / `SUPublicEDKey`.
fn verify_ed25519(message: &[u8], signature_b64: &str, pubkey_b64: &str) -> Result<()> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signature, VerifyingKey};

    let pk_bytes = STANDARD
        .decode(pubkey_b64.trim())
        .context("update public key is not valid base64")?;
    let pk_arr: [u8; 32] = pk_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("update public key must be 32 bytes, got {}", pk_bytes.len()))?;
    let vk = VerifyingKey::from_bytes(&pk_arr).context("invalid Ed25519 public key")?;

    let sig_bytes = STANDARD
        .decode(signature_b64.trim())
        .context("manifest signature is not valid base64")?;
    let sig_arr: [u8; 64] = sig_bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("Ed25519 signature must be 64 bytes, got {}", sig_bytes.len()))?;
    let sig = Signature::from_bytes(&sig_arr);

    vk.verify_strict(message, &sig)
        .context("manifest signature verification failed")
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn data_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/crowd-cast"))
}

fn bundle_dir(abi: &str) -> Result<PathBuf> {
    Ok(data_root()?.join("obs").join(abi))
}

fn work_dir() -> Result<PathBuf> {
    Ok(data_root()?.join("updates"))
}

fn state_path() -> Result<PathBuf> {
    Ok(data_root()?.join("update-state.json"))
}

/// Marker recording the binary version this machine last *ran*. Compared on the next launch so a
/// self-update can fire a "you were updated" toast — the Linux analog of the macOS `last_build`
/// marker checked in `UpdaterController::check_post_update_notification`.
fn last_run_version_path() -> Result<PathBuf> {
    Ok(data_root()?.join("last-run-version"))
}

/// Whether a version change between the previously-recorded run and now warrants a post-update
/// notification. Pure (unit-tested): never notify on first run (empty marker) or when unchanged.
fn should_notify_post_update(previous: &str, current: &str) -> bool {
    !previous.is_empty() && previous != current
}

impl InstalledState {
    fn load() -> Self {
        match state_path().ok().and_then(|p| std::fs::read(p).ok()) {
            Some(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            None => Self::default(),
        }
    }

    fn save(&self) -> Result<()> {
        let p = state_path()?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&p, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("failed to write {}", p.display()))
    }
}

// ---------------------------------------------------------------------------
// Network (blocking via a private current-thread runtime so the API stays sync,
// matching the macOS controller which takes no runtime handle)
// ---------------------------------------------------------------------------

fn http_get(url: &str) -> Result<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build updater HTTP runtime")?;
    rt.block_on(async {
        let resp = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url} returned an error status"))?;
        Ok::<_, anyhow::Error>(resp.bytes().await?.to_vec())
    })
}

fn download_verify(url: &str, expected_sha256: &str, dest: &Path) -> Result<()> {
    let bytes = http_get(url)?;
    let got = sha256_hex(&bytes);
    if !got.eq_ignore_ascii_case(expected_sha256.trim()) {
        bail!("SHA-256 mismatch for {url}: manifest {expected_sha256}, got {got}");
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(dest, &bytes).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(())
}

fn extract_bundle(archive: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest).ok();
    }
    std::fs::create_dir_all(dest)?;
    // GNU tar with zstd. The bundle tarball roots at `usr/`.
    let status = Command::new("tar")
        .arg("--zstd")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("failed to spawn `tar` to extract the libobs bundle")?;
    if !status.success() {
        bail!("`tar` failed to extract {} (status {status})", archive.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The updater
// ---------------------------------------------------------------------------

/// Mutable state behind `Arc` so a background check thread can stage an update while the
/// (synchronous) tray loop polls and applies — the rough equivalent of Sparkle's internal state.
#[derive(Debug, Default)]
struct Shared {
    staged: Mutex<Option<StagedUpdate>>,
    /// Set when an update is freshly staged; consumed by `take_prepare_for_update_request()`.
    prepare_requested: AtomicBool,
    /// A background check+stage is currently running.
    check_in_flight: AtomicBool,
    /// `apply()` is in progress — guards against re-entry from the tray's repeated `set_busy(false)`.
    applying: AtomicBool,
}

#[derive(Debug)]
pub struct LinuxUpdater {
    available: bool,
    reason: Option<String>,
    feed_url: Option<String>,
    pubkey_b64: Option<String>,
    shared: Arc<Shared>,
}

impl LinuxUpdater {
    pub fn new() -> Self {
        match (FEED_URL, PUBKEY_B64) {
            (Some(feed), Some(pk)) if !feed.is_empty() && !pk.is_empty() => {
                // Validate the baked key up front so a misconfigured build fails loudly, not at swap time.
                use base64::{engine::general_purpose::STANDARD, Engine};
                match STANDARD.decode(pk.trim()) {
                    Ok(b) if b.len() == 32 => Self {
                        available: true,
                        reason: None,
                        feed_url: Some(feed.to_string()),
                        pubkey_b64: Some(pk.to_string()),
                        shared: Arc::new(Shared::default()),
                    },
                    _ => Self::unavailable(
                        "CROWD_CAST_UPDATE_PUBKEY is not a base64-encoded 32-byte Ed25519 key.",
                    ),
                }
            }
            _ => Self::unavailable(
                "Auto-update is not configured in this build (CROWD_CAST_UPDATE_FEED_URL / CROWD_CAST_UPDATE_PUBKEY unset).",
            ),
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_string()),
            feed_url: None,
            pubkey_b64: None,
            shared: Arc::new(Shared::default()),
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    fn has_staged(&self) -> bool {
        self.shared.staged.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    fn staged_version(&self) -> Option<String> {
        self.shared
            .staged
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.version.clone()))
    }

    /// Whether the controller may start a new check (available, nothing staged or in flight).
    /// Mirrors Sparkle's `canCheckForUpdates`.
    pub fn can_check(&self) -> bool {
        self.available
            && !self.shared.check_in_flight.load(Ordering::SeqCst)
            && !self.has_staged()
    }

    /// Kick a NON-blocking background check+stage on a worker thread. The tray loop is synchronous,
    /// so the manifest fetch + (up to ~17 MB) download must not run inline. No-op if unavailable or
    /// a check/stage is already pending. Mirrors `checkForUpdatesInBackground`.
    pub fn check_in_background(self: &Arc<Self>) {
        if !self.can_check() {
            return;
        }
        if self.shared.check_in_flight.swap(true, Ordering::SeqCst) {
            return; // lost the race; another check is starting
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            if let Err(e) = me.check_and_stage() {
                warn!("Auto-update check failed: {e:#}");
            }
            me.shared.check_in_flight.store(false, Ordering::SeqCst);
        });
    }

    /// Consume the "an update is staged — please quiesce capture" request. The tray turns this into
    /// `EngineCommand::PrepareForUpdate` (mirror of Sparkle's `shouldPostponeRelaunch` path).
    pub fn take_prepare_for_update_request(&self) -> bool {
        self.shared.prepare_requested.swap(false, Ordering::SeqCst)
    }

    /// Driven by the tray each tick with `status_blocks_immediate_update(status)`. When the engine
    /// is idle (`busy=false`) and an update is staged, apply it — mirroring macOS firing the install
    /// handler on `set_busy(false)`. `apply()` re-execs on success and never returns.
    pub fn set_busy(&self, busy: bool) {
        if busy || !self.has_staged() {
            return;
        }
        if self.shared.applying.swap(true, Ordering::SeqCst) {
            return; // already applying (apply() re-execs, so this is a belt-and-suspenders guard)
        }
        let version = self.staged_version().unwrap_or_default();
        info!("Auto-update: engine idle and update {version} staged — applying");
        if let Err(e) = self.apply() {
            warn!("Auto-update: apply failed: {e:#}");
            self.shared.applying.store(false, Ordering::SeqCst);
            // Drop the broken staged update so we don't loop on it; a later check can re-stage.
            if let Ok(mut g) = self.shared.staged.lock() {
                *g = None;
            }
        }
    }

    /// One-shot at startup (called from `UpdaterController::start`, the mirror of where macOS calls
    /// `check_post_update_notification`): if the running binary's version differs from the version
    /// recorded on the previous run, we were just self-updated and re-exec'd — fire the "you were
    /// updated" toast. Always records the current version so the next launch has a baseline; never
    /// notifies on first run. Mirrors the macOS `last_build` marker, keyed on the binary's own
    /// `CARGO_PKG_VERSION` (no separate build number on Linux).
    pub fn check_post_update_notification(&self) {
        let marker = match last_run_version_path() {
            Ok(p) => p,
            Err(e) => {
                warn!("Auto-update: cannot resolve post-update marker path: {e:#}");
                return;
            }
        };

        let previous = std::fs::read_to_string(&marker).unwrap_or_default();
        let previous = previous.trim().to_string();

        // Always record the current version (mirrors macOS always writing the build marker), so a
        // later launch can detect the change exactly once.
        if let Some(parent) = marker.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Err(e) = std::fs::write(&marker, CURRENT_VERSION) {
            warn!("Auto-update: failed to write post-update marker {}: {e}", marker.display());
        }

        if should_notify_post_update(&previous, CURRENT_VERSION) {
            info!("Auto-update: detected update {previous} -> {CURRENT_VERSION}; notifying");
            // No separate build number on Linux; the version string carries the change.
            super::show_update_completed_notification(CURRENT_VERSION, "");
        }
    }

    /// Fetch + verify the manifest, and if anything changed, download + verify + stage it.
    /// Does NOT apply (mirrors Sparkle staging the update before the relaunch step).
    fn check_and_stage(&self) -> Result<()> {
        let (feed, pubkey) = match (&self.feed_url, &self.pubkey_b64) {
            (Some(f), Some(p)) => (f, p),
            _ => return Ok(()),
        };

        let manifest_bytes = http_get(feed)?;
        let sig_b64 = String::from_utf8(http_get(&format!("{feed}.sig"))?)
            .context("manifest signature file is not UTF-8")?;
        // Domain-separated: verify over `prefix || manifest_bytes`, not the raw bytes, so a
        // signature from another context that shares this key (a Sparkle enclosure) can't validate
        // here. The offline signer (bin/cc-sign-manifest) signs the same construction.
        verify_ed25519(
            &super::appcast_sig::signing_message(&manifest_bytes),
            &sig_b64,
            pubkey,
        )?;

        let manifest: Manifest =
            serde_json::from_slice(&manifest_bytes).context("failed to parse update manifest")?;

        let installed = InstalledState::load();
        let plan = plan_update(&manifest, CURRENT_VERSION, current_build(), &installed);
        if plan.nothing() {
            info!("Auto-update: up to date (version {CURRENT_VERSION})");
            return Ok(());
        }

        let work = work_dir()?;
        std::fs::create_dir_all(&work).ok();

        let mut staged = StagedUpdate {
            version: manifest.version.clone(),
            new_binary: None,
            new_bundle: None,
        };

        if plan.binary_changed {
            let dest = work.join("crowd-cast-agent.new");
            download_verify(&manifest.binary.url, &manifest.binary.sha256, &dest)?;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).ok();
            staged.new_binary = Some(dest);
        }

        if plan.bundle_changed {
            let dest = work.join("bundle.tar.zst");
            download_verify(&manifest.bundle.url, &manifest.bundle.sha256, &dest)?;
            staged.new_bundle =
                Some((manifest.bundle.abi.clone(), manifest.bundle.sha256.clone(), dest));
        }

        info!(
            "Auto-update: staged {} (build {}, binary_changed={}, bundle_changed={})",
            manifest.version, manifest.build, plan.binary_changed, plan.bundle_changed
        );
        if !manifest.notes.trim().is_empty() {
            info!("Auto-update: release notes for {}: {}", manifest.version, manifest.notes.trim());
        }
        // Forward-compat fields are parsed and surfaced, but not yet enforced (silent-by-default).
        if manifest.critical {
            info!(
                "Auto-update: {} is flagged critical (minimum_version={:?}); applying via the normal idle path",
                manifest.version, manifest.minimum_version
            );
        }
        if let Ok(mut g) = self.shared.staged.lock() {
            *g = Some(staged);
        }
        // Signal the tray to quiesce capture before we apply (consumed via
        // take_prepare_for_update_request); harmless when already idle.
        self.shared.prepare_requested.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Apply the staged update: install the bundle (if changed), swap the binary in place, record
    /// state, and re-exec. On success this never returns. Returns `Err` only if the swap/exec
    /// failed before the re-exec.
    fn apply(&self) -> Result<()> {
        let staged = self
            .shared
            .staged
            .lock()
            .ok()
            .and_then(|mut g| g.take())
            .ok_or_else(|| anyhow!("apply() called with no staged update"))?;

        let mut state = InstalledState::load();

        // 1. Bundle first: extract into its versioned dir. (Cross-ABI *activation* needs the
        //    self-provisioning binary; same-ABI updates touch nothing here.)
        if let Some((abi, sha, archive)) = &staged.new_bundle {
            let dest = bundle_dir(abi)?;
            extract_bundle(archive, &dest)
                .with_context(|| format!("failed to install libobs bundle into {}", dest.display()))?;
            state.bundle_abi = abi.clone();
            state.bundle_sha256 = sha.clone();
            info!("Auto-update: installed libobs bundle {} -> {}", abi, dest.display());
        }

        // 2. Binary: write next to the running exe (same filesystem) then rename over it. On Linux
        //    you can't truncate a running binary, but you CAN rename a new file over it; the live
        //    process keeps its old inode until we re-exec.
        if let Some(new_binary) = &staged.new_binary {
            let exe = std::env::current_exe().context("current_exe() failed")?;
            let tmp = exe.with_extension("new");
            std::fs::copy(new_binary, &tmp)
                .with_context(|| format!("failed to stage new binary at {}", tmp.display()))?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).ok();
            std::fs::rename(&tmp, &exe)
                .with_context(|| format!("failed to swap binary {}", exe.display()))?;
            state.binary_version = staged.version.clone();
            info!("Auto-update: swapped binary -> {}", exe.display());
        }

        if let Err(e) = state.save() {
            warn!("Auto-update: failed to persist update state: {e}");
        }

        // 3. Re-exec the (now updated) binary, preserving args. Mirrors Sparkle relaunching the app.
        let exe = std::env::current_exe().context("current_exe() failed")?;
        let args: Vec<String> = std::env::args().skip(1).collect();
        info!("Auto-update: re-executing {} to complete update", exe.display());
        let err = Command::new(&exe).args(&args).exec();
        Err(anyhow!("re-exec after update failed: {err}"))
    }
}

// The update loop now lives in the shared tray loop (src/ui/tray.rs), which drives this updater
// through `UpdaterController` exactly as it drives Sparkle on macOS (start / can_check /
// check_for_updates_in_background / take_prepare_for_update_request / set_busy). See updater.rs.

// ---------------------------------------------------------------------------
// Tests (pure logic: parsing, signature verify, hashing, plan, state serde)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST_JSON: &str = r#"{
        "version": "1.0.4",
        "notes": "bugfixes",
        "binary": { "url": "https://example/crowd-cast-agent", "sha256": "AABB" },
        "bundle": { "abi": "32.0.2", "url": "https://example/obs.tar.zst", "sha256": "deadbeef" }
    }"#;

    #[test]
    fn parses_manifest() {
        let m: Manifest = serde_json::from_str(MANIFEST_JSON).unwrap();
        assert_eq!(m.version, "1.0.4");
        assert_eq!(m.bundle.abi, "32.0.2");
        assert_eq!(m.binary.sha256, "AABB");
        // New/forward-compat fields default cleanly on a feed that omits them.
        assert_eq!(m.build, 0);
        assert!(!m.critical);
        assert_eq!(m.minimum_version, "");
    }

    #[test]
    fn version_ordering_and_build_tiebreak() {
        assert_eq!(parse_version("1.0.4"), (1, 0, 4));
        assert_eq!(parse_version("1.0.4-rc2"), (1, 0, 4));
        assert_eq!(parse_version("2.1"), (2, 1, 0));

        // Newer version wins.
        assert!(is_newer("1.0.3", 99, "1.0.4", 0));
        // Same version: higher build wins (a rebuild without a marketing bump).
        assert!(is_newer("1.0.4", 10, "1.0.4", 11));
        // Identical version+build is NOT newer (idempotent — no churn).
        assert!(!is_newer("1.0.4", 11, "1.0.4", 11));
        // An OLDER manifest never triggers a downgrade.
        assert!(!is_newer("1.0.4", 0, "1.0.3", 999));
        assert!(!is_newer("1.0.4", 11, "1.0.4", 10));
    }

    #[test]
    fn hex_and_sha256() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        // Known vector: SHA-256("") = e3b0c442...
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn plan_detects_what_changed() {
        let m: Manifest = serde_json::from_str(MANIFEST_JSON).unwrap();

        // Same version, same bundle hash => nothing.
        let same = InstalledState {
            binary_version: "1.0.4".into(),
            bundle_abi: "32.0.2".into(),
            bundle_sha256: "deadbeef".into(),
        };
        assert_eq!(
            plan_update(&m, "1.0.4", 0, &same),
            Plan { binary_changed: false, bundle_changed: false }
        );

        // Older binary, same bundle => binary-only (the common bugfix case).
        let binary_only = InstalledState {
            binary_version: "1.0.3".into(),
            bundle_abi: "32.0.2".into(),
            bundle_sha256: "deadbeef".into(),
        };
        assert_eq!(
            plan_update(&m, "1.0.3", 0, &binary_only),
            Plan { binary_changed: true, bundle_changed: false }
        );

        // Same version but bundle hash differs => bundle-only.
        let bundle_only = InstalledState {
            binary_version: "1.0.4".into(),
            bundle_abi: "32.0.1".into(),
            bundle_sha256: "0000".into(),
        };
        assert_eq!(
            plan_update(&m, "1.0.4", 0, &bundle_only),
            Plan { binary_changed: false, bundle_changed: true }
        );
    }

    #[test]
    fn post_update_notify_only_on_change_after_first_run() {
        // First run (no marker yet) never notifies, even though "current" is set.
        assert!(!should_notify_post_update("", "1.0.4"));
        // Unchanged version: no notification.
        assert!(!should_notify_post_update("1.0.4", "1.0.4"));
        // A real version change (the post-self-update re-exec case): notify.
        assert!(should_notify_post_update("1.0.3", "1.0.4"));
    }

    #[test]
    fn state_roundtrips() {
        let s = InstalledState {
            binary_version: "1.0.4".into(),
            bundle_abi: "32.0.2".into(),
            bundle_sha256: "abc".into(),
        };
        let bytes = serde_json::to_vec(&s).unwrap();
        let back: InstalledState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.binary_version, "1.0.4");
        assert_eq!(back.bundle_sha256, "abc");
        // Missing fields default rather than erroring.
        let partial: InstalledState = serde_json::from_str("{}").unwrap();
        assert_eq!(partial.binary_version, "");
    }

    #[test]
    fn ed25519_verify_roundtrip_and_rejects_tampering() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ed25519_dalek::{Signer, SigningKey};

        // Fixed seed so the test is deterministic (no rng feature needed).
        let seed = [7u8; 32];
        let sk = SigningKey::from_bytes(&seed);
        let vk_b64 = STANDARD.encode(sk.verifying_key().to_bytes());

        let msg = MANIFEST_JSON.as_bytes();
        let sig_b64 = STANDARD.encode(sk.sign(msg).to_bytes());

        // Correct signature verifies.
        assert!(verify_ed25519(msg, &sig_b64, &vk_b64).is_ok());

        // Tampered message is rejected.
        let mut tampered = MANIFEST_JSON.as_bytes().to_vec();
        tampered[0] ^= 0xff;
        assert!(verify_ed25519(&tampered, &sig_b64, &vk_b64).is_err());

        // Wrong key is rejected.
        let other = STANDARD.encode(SigningKey::from_bytes(&[9u8; 32]).verifying_key().to_bytes());
        assert!(verify_ed25519(msg, &sig_b64, &other).is_err());

        // Malformed key/sig are errors, not panics.
        assert!(verify_ed25519(msg, &sig_b64, "not-base64!!").is_err());
        assert!(verify_ed25519(msg, "AAAA", &vk_b64).is_err());
    }

    /// The whole point of domain separation: a manifest signed via `signing_message` verifies as a
    /// manifest, but a signature over the SAME key in another context (here: the raw bytes, standing
    /// in for a Sparkle enclosure) does NOT validate as a manifest signature, and vice-versa. This
    /// is exactly the sign/verify contract between `bin/cc-sign-manifest` and `check_and_stage`.
    #[test]
    fn domain_separated_signature_only_valid_as_manifest() {
        use super::super::appcast_sig::signing_message;
        use base64::{engine::general_purpose::STANDARD, Engine};
        use ed25519_dalek::{Signer, SigningKey};

        let sk = SigningKey::from_bytes(&[3u8; 32]);
        let vk_b64 = STANDARD.encode(sk.verifying_key().to_bytes());
        let manifest = MANIFEST_JSON.as_bytes();

        // Signer signs the domain-separated message (what cc-sign-manifest does).
        let manifest_sig = STANDARD.encode(sk.sign(&signing_message(manifest)).to_bytes());
        // Verifier checks the same construction => valid.
        assert!(verify_ed25519(&signing_message(manifest), &manifest_sig, &vk_b64).is_ok());
        // The same signature does NOT verify over the raw (un-prefixed) bytes.
        assert!(verify_ed25519(manifest, &manifest_sig, &vk_b64).is_err());

        // Conversely, a signature minted over the raw bytes (a different context, same key) is
        // rejected as a manifest signature.
        let raw_sig = STANDARD.encode(sk.sign(manifest).to_bytes());
        assert!(verify_ed25519(&signing_message(manifest), &raw_sig, &vk_b64).is_err());
    }
}
