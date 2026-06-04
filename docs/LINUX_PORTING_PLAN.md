# crowd-cast → Linux porting plan

Branch: `linux-compat` (forked from `cross-platform-support`).
Scope: bring crowd-cast's privacy-preserving, app-scoped screencast + input capture to Linux,
sequenced X11 → wlroots (Hyprland/Sway) → GNOME/KDE-Wayland.

---

## Part A — Does the branch have the right abstractions? (the "for starters" check)

`cross-platform-support` = `main` + one commit: *"refactor: extract PlatformTray trait for
cross-platform tray support."* Assessed per subsystem:

| Subsystem | Today | Pluggable? | Verdict |
|---|---|---|---|
| **Input** (`input/backend.rs`) | `trait InputBackend` + `create_input_backend()` factory; evdev/rdev already selected per-platform | ✅ trait + factory | **Ready** |
| **Tray** (`ui/platform_tray.rs`, `tray_macos.rs`) | NEW `trait PlatformTray` + `create_platform_tray()` factory + `StubTray`; macOS isolated in `tray_macos.rs` | ✅ trait + factory | **Ready** (the new commit) |
| **Frontmost app** (`capture/frontmost.rs`) | `get_frontmost_app()` cfg-dispatched; Linux=xdotool/X11, Wayland→`None` | ⚠️ free-fn + cfg | OK to extend |
| **App enumeration** (`capture/apps.rs`) | `list_running_apps()` cfg-dispatched; Linux=`/proc` scan | ⚠️ free-fn + cfg | OK to extend |
| **Permissions** (`installer/permissions.rs`) | cfg-dispatched; Linux input-group check present | ⚠️ free-fn + cfg | OK |
| **Autostart** (`installer/autostart.rs`) | cfg-dispatched; Linux XDG `.desktop` **already implemented** | ⚠️ free-fn + cfg | **Done** |
| **Notifications** (`ui/notifications.rs`) | 15 fns cfg-dispatched; non-macOS = no-op stubs | ⚠️ free-fn + cfg | needs Linux impl |
| **Updater** (`ui/updater.rs`) | Sparkle, macOS-only; non-macOS = "unavailable" | ⚠️ free-fn + cfg | defer (packaging-based) |
| **Display recovery** (`capture/recovery.rs`) | `DisplayMonitor` macOS-only; non-macOS = no-op stub | ⚠️ struct + cfg | low priority |
| **Capture** (`capture/sources.rs`, `capture/context.rs`) | macOS SCK only; `bail!("not yet implemented")`; `CaptureContext` orchestration gated on `cfg!(target_os="macos")` | ❌ **no trait** | **NOT abstracted — main work** |

**Bottom line:** The branch points in the right direction and the two stateful UI/IO seams
(input, tray) are now *properly* trait-abstracted with factories — adding a Linux impl is a new
file + one factory arm. The thin per-platform helpers (frontmost, apps, permissions, autostart,
notifications) are `cfg`-dispatched free functions; that's an acceptable pattern for small native
shims and needs no refactor, only Linux bodies.

**The gap is capture.** `sources.rs`/`context.rs` are macOS/ScreenCaptureKit-shaped with `bail!`
stubs and no trait. This is both the hardest subsystem and the one that most needs a new
abstraction before Linux work begins (see Part B).

### Blockers to even compiling on Linux (must fix first)
1. **`libobs-bootstrapper` `compile_error!("not supported on Linux")`** — it is an *unconditional*
   dependency (`Cargo.toml:19`) and imported unconditionally (`capture/context.rs:11`). The branch
   cannot build on Linux until this is gated out.
2. **No Linux OBS provisioning** — `build.rs` only runs `cargo_obs_build::install()` on macOS;
   Linux must rely on system `libobs` (pkg-config path already in `libobs/build.rs`) or bundle OBS.
3. **`no_tray` short-circuit** — `build.rs` still emits `cargo:rustc-cfg=no_tray` on Linux, and
   `main.rs` gates the whole tray block on `#[cfg(not(no_tray))]`. So `StubTray`/`create_platform_tray`
   are currently *unreachable* on Linux. Enabling a real tray means dropping `no_tray` on Linux.

---

## Part B — The one abstraction to add: `CaptureBackend`

Mirror the `PlatformTray` pattern for capture. Lift the SCK-specific orchestration out of
`CaptureContext` behind a trait so the sync engine stops being macOS-shaped:

```rust
// capture/backend.rs (new)
pub trait CaptureBackend {
    /// Build/refresh the set of scenes/sources for the given target apps.
    fn setup(&mut self, target_apps: &[String]) -> Result<()>;
    /// Activate capture of the given app (frontmost), or blank when None.
    fn set_active_app(&mut self, app_id: Option<&str>) -> Result<()>;
    fn capabilities(&self) -> CaptureCaps;
    // ... display-change / dimensions hooks reused from today's CaptureContext
}

pub struct CaptureCaps {
    pub app_capture: bool,            // capture by app identity (SCK / foreign-toplevel)
    pub window_capture: bool,         // single-window only
    pub follow_focus: bool,           // can switch captured source on focus change
    pub requires_user_picker: bool,   // portal: one consent dialog per source
    pub enforces_privacy: bool,       // non-selected pixels guaranteed excluded
}

fn create_capture_backend(...) -> Box<dyn CaptureBackend> { /* cfg + compositor detection */ }
```

Implementations, in build order:
- `MacosScreenCaptureKit` — wrap today's `CaptureContext` app-scene logic unchanged.
- `X11Composite` — `xcomposite_input` per-window (libobs source exists in the fork).
- `WaylandToplevel` — `ext-image-copy-capture` / `hyprland-toplevel-export` by handle (wlroots).
- `WaylandPortal` — xdg-desktop-portal `ScreenCast` WINDOW source + persisted `restore_token`
  (GNOME/KDE), degraded mode (`requires_user_picker=true`, `follow_focus` best-effort).

The engine's existing per-app-scene + blank-scene model maps directly onto `set_active_app`.
`capabilities()` lets the wizard and engine degrade honestly (e.g. show the per-app picker flow on
GNOME/KDE, a dialog-free tick-list on wlroots) instead of silently doing nothing.

---

## Part C — Phased plan

Targets ordered by ascending difficulty. Each phase is independently shippable.

### Phase 0 — Make it build & run on Linux (no capture yet)
- Gate `libobs-bootstrapper` to `cfg(not(target_os="linux"))` in `Cargo.toml` + `context.rs`;
  add a Linux init path that uses system `libobs` (pkg-config) and skips bootstrap.
- Decide OBS provisioning: **(a)** depend on distro `libobs` (simplest, dev), **(b)** bundle via
  Flatpak/AppImage (shipping). Recommend (a) for dev, (b) for release.
- Confirm `libobs/build.rs` Linux pkg-config link works; install `libobs-dev` + plugins
  (`linux-pipewire`, `linux-capture`, `obs-ffmpeg`, `obs-x264`).
- Result: agent compiles, runs headless (`no_tray`), input capture works, uploads work.
- Effort: ~2–4 days.

### Phase 1 — Input (mostly done)
- evdev backend already reads true `REL_X/REL_Y` deltas. Tasks: keep evdev for **X11 too** (rdev's
  X11 `listen` gives absolute, not deltas — switch X11 to evdev or XInput2 raw motion); improve
  keycode→name mapping (XKB); harden the `input`-group permission UX (already detected in
  `permissions.rs`, surface it in the wizard).
- Effort: ~2–3 days.

### Phase 2 — Focus / frontmost
- Replace the `xdotool` shell-out with direct X11 `_NET_ACTIVE_WINDOW`.
- Wayland focus (for follow-focus): wlroots → `wlr-foreign-toplevel` `activated` / Sway-Hyprland IPC;
  GNOME → shell extension; KDE → KWin D-Bus. Compositor-specific; wire behind `frontmost.rs`.
- Effort: X11 ~1 day; wlroots IPC ~2–3 days; GNOME/KDE focus ~3–5 days.

### Phase 3 — Capture backend (the core work)
- Introduce `CaptureBackend` trait (Part B); refactor `CaptureContext` into `MacosScreenCaptureKit`.
- **X11 first:** `X11Composite` via `xcomposite_input` — per-window capture (privacy holds, no
  occlusion leak), map target app → window(s), follow focus. Closest to full macOS parity.
- **wlroots next:** `WaylandToplevel` — enumerate via `ext-foreign-toplevel-list`, capture by handle
  (`hyprland-toplevel-export-v1` / `ext-image-copy-capture-v1`); fully programmatic, no picker.
- **GNOME/KDE last:** `WaylandPortal` — WINDOW source + `persist_mode=2` `restore_token` per app
  (one consent dialog per app during the wizard, then silent restore); follow-focus best-effort via
  Phase-2 focus signal; multi-app = N persisted sessions.
- Effort: trait+macOS refactor ~3–4 days; X11 ~1–1.5 wk; wlroots ~1.5–2 wk; portal ~2–3 wk.

### Phase 4 — Tray (glanceable status)
- Drop `no_tray` on Linux in `build.rs`; add `cfg(target_os="linux") → LinuxTray` arm to
  `create_platform_tray()`.
- Implement `LinuxTray` against the `PlatformTray` trait. **Note:** the `deps/tray` C library is
  *not* committed to the repo, so this is greenfield — recommend a Rust-native StatusNotifierItem
  crate (`ksni`, or `tray-icon`) over vendoring GTK/AppIndicator C. Reuse the existing icon-state
  swap logic (Idle/Recording/Blocked) — the green/red glanceable status maps directly.
- Effort: ~3–5 days (KDE/AppIndicator easy; GNOME needs AppIndicator extension — document it).

### Phase 5 — Notifications & permissions
- `notifications.rs`: implement the Linux bodies via `org.freedesktop.Notifications` (libnotify or
  `notify-rust`). 15 thin functions → straightforward.
- Permissions/autostart: input-group check and `.desktop` autostart already implemented — verify.
- Effort: ~2–3 days.

### Phase 6 — Updater (defer)
- No Sparkle on Linux. Use packaging-native updates (Flatpak/distro repo) or skip auto-update for v1.
- Effort: deferred / packaging-dependent.

### Phase 7 — Packaging, distribution, CI
- Pick a format: Flatpak (best portal/sandbox story, bundles libobs), AppImage, or `.deb`.
- Add Linux to CI build matrix (today `scripts/` + `.github/` are macOS/Sparkle-only).
- Effort: ~1–2 wk.

---

## Recommended sequencing & rough totals (with agent assistance)
1. **Phase 0–1** (build + input): ~1 wk → a headless Linux agent that captures input and uploads.
2. **X11 capture + tray** (Phase 2-X11, 3-X11, 4): ~2–3 wk → near-parity on X11.
3. **wlroots capture** (Phase 3-wlroots): ~1.5–2 wk → full programmatic parity on Hyprland/Sway.
4. **GNOME/KDE Wayland** (Phase 3-portal + focus): ~3–4 wk → degraded picker mode.
5. **Notifications, packaging, CI**: ~1.5–2 wk.

≈ **2–3 months** to bring all Linux environments to today's macOS bar; an **X11-only MVP in ~3–4 wk**.

## Open decisions for the team
- OBS provisioning on Linux: system `libobs` vs bundle (Flatpak/AppImage)?
- Which Wayland compositors are in-scope for v1? (Recommend X11 + wlroots first; GNOME/KDE portal
  mode as a follow-up given the per-app picker UX and protocol immaturity.)
- Tray: Rust-native SNI crate (`ksni`/`tray-icon`) vs vendoring `deps/tray` C?
- Auto-update strategy on Linux (or punt to packaging)?
