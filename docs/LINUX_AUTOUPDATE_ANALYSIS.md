# Linux auto-update — analysis & options

Status: **research / decision doc** (no code yet). Answers the open question in
`LINUX_PORTING_PLAN.md` ("Auto-update strategy on Linux (or punt to packaging)?").

## TL;DR

- There is **no "Sparkle for Linux"** — no OS- or DE-level appcast+updater framework. The
  ecosystem's explicit analogue is **AppImage + AppImageUpdate (zsync)**, which the AppImage
  project itself describes as aiming for "a UX roughly similar to Sparkle."
- On Linux, **choosing an auto-updater is really choosing a packaging format first** — and we
  haven't picked one yet (the app is run from source; only *libobs* is bundled). The two are
  coupled, so this doc treats them together.
- **Recommendation:** ship Linux as an **AppImage** and self-update it via **zsync delta
  updates**, with our **own Ed25519 signature check layered on top** (reusing the Sparkle
  keypair's algorithm). This is the only option that simultaneously fits *all* our hard
  constraints: no root, raw evdev (no sandbox), a ~100 MB native bundle, S3 hosting, in-app
  self-triggered updates, and delta downloads. It also **solves Linux packaging at the same
  time** — and our existing glibc-2.28/leaf-libs-only bundle work *is already the AppImage
  philosophy*, so we're ~80% there.
- **Prerequisite / coupling:** the updater is driven from the tray event loop, and Linux is
  currently `no_tray`. Auto-update can't ship before the Linux tray (or an equivalent driver
  loop) exists.

---

## 1. How macOS works today (the model we're matching)

`UpdaterController` (`src/ui/updater.rs`) wraps a static ObjC shim (`updater_darwin.m`) that
links `Sparkle.framework` (gated by the build-time `has_sparkle` cfg). The release pipeline
(`scripts/release-macos.sh`):

1. Build + sign the `.app`; embed `SUFeedURL` + `SUPublicEDKey` in `Info.plist`.
2. Notarize + staple; build a DMG.
3. `ditto`-zip the `.app`; run Sparkle's `generate_appcast` to produce an **EdDSA (Ed25519)-signed**
   `appcast.xml`.
4. Host the ZIP archives + `appcast.xml` on **S3**.

At runtime: the tray's "Check for updates" + a 600 s background poll call into Sparkle; a
`PrepareForUpdate` handshake cleanly stops the capture engine before install; a `last_build`
marker file drives a "you were updated" notification on next launch.

**Reusable assets for Linux:** the Ed25519 keypair, the S3 hosting, the platform-neutral
`UpdaterController` API consumed by `tray.rs`, the clean-stop-before-install handshake, and the
post-update-notification pattern. **Not reusable:** Sparkle itself (macOS-only), the `.app`
bundle layout, `Info.plist`/`PlistBuddy` version reads.

---

## 2. The reframe: on Linux, "auto-update" ≈ "packaging format"

Unlike macOS (where a relocatable `.app` + Sparkle is *the* answer), Linux update mechanisms are
bound to how you ship the app:

| If you ship as… | …the update mechanism is… |
|---|---|
| AppImage | AppImageUpdate / zsync (self-triggered, delta, S3) |
| Flatpak | OSTree pull (store-driven background, delta) |
| Snap | snapd forced auto-refresh (store-locked) |
| `.deb`/`.rpm` + repo | apt/dnf (root, admin-driven) |
| Plain binary + sidecar | roll-your-own (self-replace + signed manifest) |

So the decision below is a joint packaging+update decision. Today crowd-cast on Linux has **no app
packaging** — it builds from source, and only *libobs* ships as a relocatable
`obs-bundle-*.tar.zst`. Picking the updater also picks the distribution format.

---

## 3. Our hard constraints (the filter)

1. **No root** assumed for install or update.
2. **Raw evdev** input capture → needs `/dev/input` access (sandbox-hostile). Today provisioned
   once via udev rule + `input` group (the wizard's job).
3. **~100 MB+ native bundle** (libobs + ffmpeg + plugins), changes little release-to-release →
   **delta updates matter a lot**.
4. **Cross-distro**, glibc-2.28 floor (already engineered in `packaging/linux/`).
5. **S3 + GitHub Releases** hosting already in place.
6. **Ed25519 signing** already in place (Sparkle EdDSA keypair).
7. Wants the **in-app, self-triggered, tray-driven** UX we already have on macOS.
8. Wayland-first: screen capture via the **PipeWire ScreenCast portal**.

---

## 4. Options, scored against the constraints

### A. AppImage + AppImageUpdate (zsync)  — **recommended**

One relocatable `.AppImage` (our binary + the libobs bundle inside an AppDir) + a sibling
`.zsync` file on S3. `libappimageupdate` (or `appimageupdatetool`) checks the embedded
update-info URL, downloads **only changed blocks** via HTTP range requests, writes a *new* file
beside the running one, and applies on next launch.

- **No root, no sandbox** → raw evdev works exactly like a native binary. ✅ (1, 2)
- **Big bundle is the norm**; AppImage's "build on oldest glibc, bundle leaf libs, let the host
  provide GL/Mesa/PipeWire/glibc" philosophy is **identical to what `packaging/linux/README.md`
  already does** — minimal extra work to wrap into an AppDir. ✅ (3, 4)
- **zsync delta** → a 100 MB bundle that barely changes downloads as a few MB. ✅ (3)
- **S3-hostable** (zsync needs HTTP range requests, which S3 supports); GitHub-Releases transport
  also exists. ✅ (5)
- **Self-triggerable** from our Rust code via `libappimageupdate` → maps onto the existing
  `UpdaterController` methods. ✅ (7)
- Closest thing to **Sparkle for Linux**.

**Caveats to manage:**
- **Signing.** AppImage's *native* signing is **GPG**, and its built-in validation is *not*
  turnkey/key-pinning (the docs warn `--appimage-signature` only *displays* a sig). Don't rely on
  it. Instead **layer our own Ed25519 check**: publish an Ed25519-signed manifest, verify the
  assembled AppImage against it before relaunch. This reuses our Sparkle key's algorithm. ⚠️ (6)
- **FUSE.** Classic AppImages need FUSE; Ubuntu 24.04 renamed `libfuse2`→`libfuse2t64` and
  restricts unprivileged user namespaces. Use a FUSE3 / no-FUSE-extract runtime and test the
  distro matrix.
- **AppImageUpdate maturity:** actively developed but perpetually labelled alpha/beta; we own the
  integration regardless (as we do on macOS).
- **Wayland:** the ScreenCast portal **restore token is single-use** and must be re-acquired +
  re-persisted on every relaunch — handle this in the post-update restart path.

### B. Hand-rolled in-app updater (Tauri/minisign design + `self-replace`) — **strongest infra reuse**

Keep the binary + sidecar-dir layout. Host an **Ed25519-signed JSON manifest** (Tauri-updater
schema: `version` / `platforms.linux-x86_64.{url,signature}` / `pub_date` / `notes`) on S3 or
GitHub Releases; verify with a minisign/Ed25519 crate (`minisign-verify`); download the new binary
+ bundle tarball; verify; swap the binary via the **`self-replace`** crate
(unlink/rename-over the inode — legal while running) and atomically `rename` the new bundle dir
into place; then re-exec cleanly.

- **Maximum reuse**: Ed25519 keys (6), S3/GitHub (5), the existing in-app/tray UX (7), the
  clean-stop handshake, and the `last_build` notification all carry over almost 1:1. This is the
  truest architectural mirror of the macOS Sparkle flow.
- **No off-the-shelf delta** — full re-download of the 100 MB bundle each release unless we add
  our own binary-diff step. ✗ (3)
- **We build & maintain** the download/verify/swap/restart/sidecar logic ourselves; and it
  **doesn't solve packaging** (we'd still need a separate "how do users first get it" story).
- Linux executable swap-while-running is well-understood: can't truncate a running binary
  (`ETXTBSY`), but unlink + rename-over works; the in-memory process keeps old code until re-exec.

> Note `self_update` (Rust crate) gives the S3/GitHub fetch plumbing for this design, and
> `cargo-dist`/`axoupdater` is the *only* turnkey option that natively understands a
> "binary + library bundle" install — **but** axoupdater verifies via checksums/attestation
> (not our Ed25519), is GitHub-Releases-only for the update path, has no delta, and its commercial
> backer appears wound down (OSS still shipping). Good on layout, weak on our Ed25519 goal.

### C. Flatpak (self-hosted OSTree) — **best Wayland + auto-update story, but fights evdev**

- **Best truly-automatic background updates** (GNOME Software / KDE Discover / `flatpak update`
  systemd timer) and **OSTree delta** updates; **first-class PipeWire ScreenCast portal**
  (genuinely nice for our Wayland capture). ✅ (8)
- **But raw evdev has no portal** → forces `--device=all`, which largely defeats the sandbox we'd
  be adopting it for. ✗ (2)
- A 100 MB proprietary libobs bundle is a poor **Flathub** citizen (`extra-data` is "last resort,
  heavily scrutinized") → realistically **self-host an OSTree repo** (S3-hostable, GPG-signed),
  which forfeits the "auto-updates for everyone via the store" upside unless users add our remote.
- Tray + autostart need explicit portal plumbing (`RequestBackground`, StatusNotifier talk-name).
- Conflicts with our deliberate libobs provisioning split (bundle leaf libs, host provides
  GL/Mesa/PipeWire) — Flatpak wants its runtime model instead.
- **Verdict:** strong longer-term option *if* we accept `--device=all` and self-host OSTree;
  secondary to AppImage for now.

### D. Snap — **avoid**

Forced auto-refresh you can't fully disable (bad for controlled rollout of a capture agent);
evdev → **classic confinement** → **manual Canonical review** + `--classic` install friction; no
real self-hosting (Snap Store / paid brand store only) — conflicts with our S3 + self-controlled
model.

### E. `.deb`/`.rpm` + apt/dnf repo — **secondary convenience channel at most**

This is the **dominant industry pattern** (Chrome, VS Code, Slack, 1Password, Spotify, Signal,
Zoom all auto-install a vendor repo + GPG key and let the package manager update) — *because it
offloads the privileged swap to trusted machinery*. But it **needs root**, doubles our packaging
surface (`.deb` *and* `.rpm`, per-distro-family quirks), and hands update control to the admin
(`unattended-upgrades`), not the app. Conflicts with our no-root, cross-distro, single-bundle
ethos. Fine as an *optional* channel for users who prefer native packages; not the primary
updater.

### Comparison

| Criterion | A. AppImage+zsync | B. Hand-rolled | C. Flatpak | D. Snap | E. deb/rpm |
|---|---|---|---|---|---|
| No-root install | ✅ | ✅ | ✅ (`--user`) | ✅ | ❌ root |
| Raw evdev (no sandbox fight) | ✅ | ✅ | ⚠️ `--device=all` | ❌ classic+review | ✅ |
| 100 MB bundle normal | ✅ | ✅ | ⚠️ discouraged | ⚠️ | ⚠️ /opt |
| Delta updates | ✅ zsync | ✗ (DIY) | ✅ OSTree | ✅ | ✅ |
| Host on our S3 | ✅ | ✅ | ✅ (OSTree) | ❌ | ✅ |
| Reuse Ed25519 keys | ⚠️ layer it on | ✅ best | ✗ (GPG) | ✗ | ✗ (GPG) |
| In-app self-triggered (Sparkle-like) | ✅ | ✅ | ⚠️ store-driven | ❌ | ❌ |
| Truly automatic/background | ⚠️ app-driven | ⚠️ app-driven | ✅ best | ✅ forced | ⚠️ if admin opts in |
| Solves Linux packaging too | ✅ | ✗ | ✅ | ✅ | ✅ |
| Build/maintenance cost | low–med | **high** | med–high | med | high (per-distro) |

---

## 5. Recommendation

**Primary: AppImage + zsync, with our own Ed25519 manifest verification.**

Rationale: it's the only option that clears every hard constraint at once, it *reuses the
relocatable-bundle work we've already done* (the glibc-2.28 container build is the AppImage
philosophy), it gives **delta updates** (which matter a lot for a 100 MB bundle), it keeps the
**in-app, tray-driven, self-triggered** UX that mirrors macOS, and it **solves the still-open
Linux packaging question in the same stroke**. The signing gap (AppImage = GPG) is closed by
layering an Ed25519-signed manifest check — same algorithm as our Sparkle key.

**If maximal architectural symmetry with macOS matters more than delta/packaging**, option B
(hand-rolled minisign/Ed25519 + `self-replace`) is the closer 1:1 mirror and reuses the Ed25519
keys most directly — at the cost of building the swap/restart/delta machinery ourselves and still
needing a separate packaging story. A pragmatic hybrid is **AppImage as the artifact + our
Ed25519 manifest as the trust anchor** (use zsync for the efficient download, verify with our key
before relaunch).

**Longer-term, consider Flatpak** as a second channel for users who want store-managed automatic
updates and for its first-class ScreenCast portal — accepting `--device=all` and a self-hosted
OSTree repo. **Avoid Snap.** Offer **`.deb`/`.rpm`** only as an optional native-package channel,
never the primary updater.

---

## 6. What it touches in our code

- **`src/ui/updater.rs`** — add a `#[cfg(target_os = "linux")]` `UpdaterController` backend
  implementing the same surface (`new/start/is_available/reason/can_check_for_updates/
  check_for_updates[_in_background]/take_prepare_for_update_request/set_busy`). `tray.rs` already
  consumes these platform-neutrally — no business-logic changes needed there.
- **Prerequisite — Linux tray (`no_tray` in `build.rs`).** The 600 s poll + "Check for updates"
  action + `PrepareForUpdate` handshake all live in the tray event loop, which doesn't run on
  Linux yet. Auto-update needs that loop (a Linux tray, or a headless driver) first.
- **`src/ui/mod.rs`** — `current_app_bundle_path()` is macOS-only; add a Linux notion of "am I
  running from an updatable AppImage?" (check the `$APPIMAGE` env var AppImage sets) to gate
  availability, mirroring the read-only-volume / bundle checks.
- **Post-update notification** — the `last_build` marker logic reads `Info.plist` via
  `PlistBuddy`; the Linux version reads the version from the binary (`CARGO_PKG_VERSION`/build env)
  instead.
- **Release pipeline** — new `scripts/release-linux.sh` + CI job: build in the AlmaLinux-8
  container, assemble the AppDir (binary + existing libobs bundle), run `appimagetool -u` to emit
  `.AppImage` + `.zsync`, sign an Ed25519 manifest, upload to S3 — paralleling
  `release-macos.sh` / `generate-appcast.sh`.
- **Privileged step stays separate** — the udev rule + `input`-group setup (the wizard) is a rare,
  root-gated, first-run action. A user-space AppImage/binary swap **preserves** evdev access, so
  routine updates need no root. Only changing the udev rule/group needs `pkexec` again.

---

## 7. Suggested phasing

1. **Land the Linux tray** (drop `no_tray`) — unblocks the updater driver loop. (Independent of
   this decision; already needed.)
2. **Package as AppImage** — wrap binary + libobs bundle into an AppDir; produce a signed
   `.AppImage`. (Solves "how do Linux users install it" regardless of auto-update.)
3. **Add the Linux `UpdaterController` backend** — zsync check/download via `libappimageupdate`
   (or shell `appimageupdatetool`), Ed25519 manifest verify-before-apply, wire into the existing
   tray poll + `PrepareForUpdate` clean-stop + relaunch (re-acquire ScreenCast restore token).
4. **CI/release automation** — `release-linux.sh` + GH Actions job; host on S3 alongside the
   macOS appcast.
5. **(Later, optional)** Flatpak and/or `.deb`/`.rpm` as secondary channels.

---

## 8. Open risks / to verify

- FUSE3 / no-FUSE behavior + Ubuntu 24.04 user-namespace restrictions across our distro matrix.
- Confirm whether to use `libappimageupdate` (link) vs shelling out to `appimageupdatetool`.
- ScreenCast restore-token re-acquisition on post-update relaunch (single-use token).
- Decide the trust anchor precisely: AppImage GPG vs our Ed25519 manifest vs both.
- Whether to invest in delta tooling for option B if we go that route instead.
