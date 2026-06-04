# Linux handoff — build, run, and validate (for the Linux-machine agent)

This branch (`linux-compat`) makes crowd-cast **compile and run on Linux with GPU-accelerated
capture + encoding**, structured so Windows can be added by mirroring the Linux code. It was
authored on macOS, where the `#[cfg(target_os="linux")]` paths **cannot be compiled** — so they
are verified by review against the libobs-rs fork APIs, not by compilation. Your job is to compile,
run, and validate on Linux, and fix anything that surfaces.

The macOS build was confirmed green (`cargo check`, exit 0) after these changes, so the shared and
macOS code is unaffected.

---

## CRITICAL #1 — GPU/VAAPI fix (DONE — already wired in)

GPU **encoding** on Intel/AMD Linux requires VAAPI encoders in libobs-simple's hardware candidate
list (without them, Intel/AMD GPUs silently fall back to software x264). This is already done and
wired in — **no action needed**:

- Fork branch **`p-doom/libobs-rs` `linux-support`** (forked from `macos-support`) contains the fix
  in `libobs-simple/src/output/simple.rs::hardware_candidates` (`FFMPEG_VAAPI[_TEX]`,
  `HEVC_FFMPEG_VAAPI[_TEX]`, `AV1_FFMPEG_VAAPI[_TEX]`). It is **pushed**.
- crowd-cast's `Cargo.toml` points all `libobs-*` deps at `linux-support`, and `Cargo.lock` pins the
  rev containing the fix (`81f8498`). Verified to compile on macOS.
- `macos-support` was intentionally left untouched and unpushed.

Just confirm at runtime that a hardware encoder is selected (see "Validate GPU" below).

## ⚠️ CRITICAL #2 — host infrastructure cannot be bundled

libobs is bundled/provisioned, but these must exist on the host (see
`docs/LINUX_LIBOBS_PROVISIONING.md`): the GPU userspace driver (Mesa/NVIDIA GL+EGL) and `/dev/dri`;
for Wayland capture, a running **PipeWire daemon + xdg-desktop-portal + a compositor-matching
backend + D-Bus session**; for VAAPI, `libva` + a VA driver (optional — x264 fallback otherwise).

---

## What this commit changes

- **`Cargo.toml`**: `libobs-bootstrapper` moved to `[target.'cfg(not(target_os = "linux"))'.dependencies]`
  (it hard-`compile_error!`s on Linux and is unneeded there).
- **`src/capture/context.rs`**: bootstrapper usage gated out on Linux; `CaptureContext::new` skips
  bootstrap on Linux; added a Linux `obs_startup_paths_from_env()` driven by `CROWD_CAST_OBS_*` env
  vars; `initialize()` applies env startup paths on Linux too.
- **`src/capture/sources.rs`**: Linux capture sources implemented —
  - display: PipeWire desktop (Wayland) / XSHM (X11);
  - per-window: PipeWire window (Wayland) / XComposite (X11);
  - plus Windows stubs with exact recipes and an unsupported-platform fallback.
- **fork `libobs-simple`**: VAAPI encoder candidates (see CRITICAL #1).

GPU **rendering** needs no code change: libobs-wrapper already selects the EGL nix platform
(X11-EGL / Wayland) and links the host GL stack.

---

## Build prerequisites (Linux build machine)

The `libobs` crate links libobs at **build time** via pkg-config (or `LIBOBS_PATH`). Provide one of:
- `sudo apt install libobs-dev` (Ubuntu 24.04+) / distro equivalent, **or**
- point at a bundle's libobs: `export LIBOBS_PATH=/path/to/bundle/lib`

Also required at build time (unchanged from macOS): `CROWD_CAST_API_GATEWAY_URL`.

```bash
CROWD_CAST_API_GATEWAY_URL="https://.../prod/presign" cargo build --release
```

The tray is currently disabled on Linux (`no_tray` in build.rs) — out of scope for this commit; the
agent runs headless (Ctrl+C to exit). See `docs/LINUX_PORTING_PLAN.md` Phase 4 for the tray.

---

## Pointing crowd-cast at libobs (runtime)

Two supported modes (see `obs_startup_paths_from_env` in context.rs):
1. **System OBS**: install `obs-studio` (provides `/usr/share/obs` + `/usr/lib/<arch>/obs-plugins`);
   set no env — libobs-wrapper's default Linux StartupPaths are used.
2. **Relocatable bundle**: set all three (point at the extracted bundle; layout per
   `docs/LINUX_LIBOBS_PROVISIONING.md`):
   ```bash
   export CROWD_CAST_OBS_DATA_PATH=/path/to/bundle/data/libobs
   export CROWD_CAST_OBS_PLUGIN_BIN_PATH=/path/to/bundle/obs-plugins/64bit
   export CROWD_CAST_OBS_PLUGIN_DATA_PATH=/path/to/bundle/data/obs-plugins/%module%
   ```
   (The download-on-first-run provisioning of this bundle is a separate task — see the provisioning
   doc. For first validation, mode 1 (system OBS) is fastest.)

Required OBS plugins present (system or bundle): `linux-pipewire`, `linux-capture`, `obs-ffmpeg`,
`obs-x264`, `obs-outputs`.

---

## Validate GPU acceleration is actually used

1. **Encoder**: run with `RUST_LOG=debug` and confirm the selected encoder is a hardware one
   (`ffmpeg_vaapi[_tex]` on Intel/AMD, `obs_nvenc_*` on NVIDIA) — **not** `obs_x264`. If it's x264 on
   an Intel/AMD box, CRITICAL #1 (fork push + cargo update) was skipped, or `libva`/VA-driver is
   missing on the host.
2. **Render**: confirm libobs uses EGL/GPU, not software. Check OBS logs for the GL renderer, and
   `LD_DEBUG=libs ./crowd-cast-agent 2>&1 | grep -E 'libGL|libEGL|libstdc'` — these MUST resolve to
   `/usr/...` (host), never a bundle dir (a bundled libGL/libstdc++ causes the `swrast` software
   fallback; see provisioning doc).

---

## Known limitations / what still needs Linux validation

- **Display capture** (empty `target_apps`) is the high-confidence path: Wayland portal desktop /
  X11 XSHM, GPU render + encode. Validate this first.
- **Per-app capture** is scaffolded but needs validation:
  - X11 XComposite expects `bundle_id` to be an **X11 window id**; mapping app→window-id is not yet
    implemented (the current multi-app call passes the configured app identifier, which won't resolve
    to a window). Implement app→window resolution, or test with display capture.
  - Wayland per-window uses the portal **picker** (user-driven) + restore-token persistence — the
    async session lifecycle is not exercised here. See `docs/LINUX_PORTING_PLAN.md`.
- **`get_main_display_resolution`** returns `Err` on Linux → `initialize()` falls back to OBS's
  default canvas (capture still works; libobs scales). Implement RANDR/wl_output query for native res.
- **Input capture** (evdev) on Wayland needs the user in the `input` group (already detected in
  `permissions.rs`).

---

## Suggested validation order
1. Install `obs-studio` (mode 1) + `libobs-dev`. `cargo build --release` (the VAAPI fix is already
   pinned via the `linux-support` branch — CRITICAL #1 is done).
2. Run with `RUST_LOG=debug`, empty `target_apps` → confirm display capture records a file.
3. Confirm hardware encoder chosen + EGL render (Validate GPU section).
4. Then iterate on per-app capture and the bundle/download provisioning.
