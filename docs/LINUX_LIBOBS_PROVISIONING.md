# Linux libobs provisioning plan (download-on-first-run, self-built bundle)

Branch: `linux-compat`. Goal: mirror the macOS/Windows **bootstrapper mechanism** — download a
self-contained artifact on first run, extract to user space, load it — while **supplying the
artifact ourselves** (OBS publishes no relocatable Linux build). Users never install OBS; no root.

This plan is grounded in (a) inspection of `libobs-wrapper`'s Linux init in the p-doom fork and
(b) web research on portable-libobs failure modes (sources at bottom).

---

## 0. The governing rule (read this first)

> **Bundle client/userspace logic. Leave anything that talks to a kernel driver or a host daemon to the host.**

We bundle libobs + our plugins + ffmpeg + x264 + data. We must **not** bundle the GPU/GL stack, the
VAAPI driver, PipeWire/D-Bus, or the C/C++ runtime — bundling those silently breaks the *host's* GPU
driver (drops to software `swrast`) or the capture IPC. This boundary is the whole design.

**Honest limit of "self-contained" on Linux:** we can bundle libobs, but we **cannot** bundle the GPU
driver, the **PipeWire daemon**, or the **xdg-desktop-portal + backend**. So Wayland capture still
requires host infrastructure to be present (it is, on any normal desktop; it is *not* on minimal/
headless/odd setups). Unlike macOS (frameworks + SCK fully shipped), the Linux agent depends on host
graphics/capture infra even with libobs bundled. We must detect and report this, not assume it.

---

## 1. Bundle-vs-host matrix (the core spec)

| Component | Verdict | Why / failure mode if wrong |
|---|---|---|
| `libobs.so`, `libobs-opengl.so`, our plugins (`linux-pipewire`, `linux-capture`, `obs-ffmpeg`, `obs-x264`, `obs-outputs`) | **BUNDLE** | Our code. Stamp `$ORIGIN` RUNPATH on each. |
| libobs **data dir** (`data/libobs/*.effect`) + per-plugin `data/` | **BUNDLE** | libobs aborts video init without `default.effect` etc. |
| ffmpeg libs (`libav*`,`libsw*`), `libx264`, output protocol libs (`librtmp`,`libsrt`,TLS) | **BUNDLE** | dlopen/transitive — `ldd` misses them; include manually. |
| **glibc** (`libc`,`ld-linux`,`libpthread`,`libm`,…) | **HOST** | Bundling kills forward-compat; dual-glibc corrupts thread state. → `GLIBC_2.xx not found`. |
| **`libstdc++` / `libgcc_s`** | **HOST** (default) | Old bundled copy → host Mesa can't resolve `GLIBCXX_*` → **swrast**. Bundle only via checkrt "newer-wins" if built with newer GCC than floor. |
| **`libGL`/`libEGL`/`libGLX`/`libGLdispatch`/`libglvnd`/`libgbm`/`libglapi`** + Mesa/NVIDIA DRI drivers | **HOST** | glvnd dispatches to host vendor; bundling → `libGL error: failed to load driver: swrast`. |
| **`libdrm`** | **HOST** | Kernel-ioctl coupled; shared by GL + VAAPI. |
| **`libva` + VA drivers** (`iHD`/`radeonsi`/…) | **HOST** | libva guarantees API, *not* ABI; driver is GPU/kernel-specific. Newer bundled libva vs old host driver → `vaInitialize` fails. |
| **`libpipewire-0.3`**, SPA plugins | **HOST** (prefer) | Must match running PipeWire daemon. Conservative bundle only as fallback. |
| **`libdbus-1`** | **HOST** | Needed to reach host portal + session bus; stable/ubiquitous. |
| **`libX11`/`libxcb`/`libwayland-client`/`libwayland-egl`** | **HOST** | Shared with GL stack + `linux-capture`; classic shadowing victim. |
| `libasound`/`libjack` (only if audio) | **HOST** | Must match host audio server ABI. |

---

## 2. Build pipeline (CI) — produce the relocatable bundle

1. **Base image = oldest glibc we support** (decision: Ubuntu 22.04 vs 20.04; manylinux-style). All
   compiled objects (libobs, plugins, ffmpeg, x264) built here. Audit max symbol with
   `objdump -T | grep GLIBC_ | sort` and beware the glibc-2.34 lib-merge bump.
2. **Obtain libobs + plugins matching the OBS major our `libobs` bindings target** (fork = OBS 32.x).
   Two options (decision): build OBS from source via CMake (headless, **no Qt frontend/CEF**), or
   extract+trim from a known-good portable build (e.g. `wimpysworld/obs-studio-portable`).
3. **Gather deps with `linuxdeploy`**, but apply the **AppImage excludelist** so the host-coupled
   libs in §1 are NOT pulled in. Scope the bundle to *leaf* media libs only.
4. **Manually add dlopen'd deps `ldd` misses** (ffmpeg codecs, x264, output/TLS libs). **Verify
   empirically** with `strace -f -e trace=openat` on a real capture+encode and diff opened `.so`s.
5. **Relocate**: `patchelf --set-rpath '$ORIGIN'` on **every** object (RUNPATH is non-transitive, so
   top-level-only fails). Decision: RUNPATH+`LD_LIBRARY_PATH` vs transitive `DT_RPATH` — pick one and
   apply uniformly.
6. **Package as a tarball of the AppDir tree** (we consume by extraction, not as a runnable
   `.AppImage` — we `dlopen` libobs into our process, we don't launch OBS via `AppRun`).
7. Per-arch: `x86_64`, `aarch64`. Emit bundle + `.sha256` (+ signature).

---

## 3. Distribution

- Host versioned bundles on **`p-doom/libobs-builds`** releases (mirrors the Windows `sshcrack/
  libobs-builds` pattern — for Linux *we* are the build maintainer by necessity).
- Version by OBS `major.minor` to satisfy the bootstrapper's `is_compatible_major`
  (`LIBOBS_API_MAJOR_VER`) check. Point crowd-cast at it via `ObsBootstrapperOptions::set_repository()`
  / our own URL.

---

## 4. Runtime provisioning (download-on-first-run, no root)

On first run (Linux), if `~/.local/share/crowd-cast/obs/<version>/` is absent:
download from our feed → **verify checksum/signature** → **atomic extract** (temp dir + rename) →
record version. Handle: partial downloads, **concurrent launches** (lockfile), **version bump**
(re-provision new dir, GC old), **offline first-run** (clear actionable error; optional
`--obs-dir`/manual path). All in user space.

---

## 5. libobs-wrapper integration (the code wiring)

- **Override `StartupPaths`** to point at the extracted bundle — *required*, because the fork's Linux
  defaults point at the **system** install (`/usr/share/obs/libobs`,
  `/usr/lib/<arch>/obs-plugins/%module%`, `/usr/share/obs/obs-plugins/%module%`,
  see `utils/info/startup.rs:166-175`). We set:
  - `libobs_data_path = <bundle>/data/libobs`
  - `plugin_bin_path  = <bundle>/obs-plugins/%module%` (or `64bit` layout — match what we ship)
  - `plugin_data_path = <bundle>/data/obs-plugins/%module%`
  Use **absolute, normalized paths (no trailing slash)** to avoid the module double-load bug
  (OBS PR #12042).
- **Patch `get_linux_opengl_lib_name()`** (`utils/linux/mod.rs`): today it reads `/usr/bin/obs` to
  discover the `libobs-opengl.so` filename and falls back to `"libobs-opengl.so"`. For our bundle it
  must resolve to *our* bundled `libobs-opengl.so`. → small **fork change** (accept an override path /
  check the bundle dir before `/usr/bin/obs`).
- **Env, set BEFORE `obs_startup` (and before process start for `LD_LIBRARY_PATH`)**: prefer relying
  on `$ORIGIN` RUNPATH so we avoid a blanket `LD_LIBRARY_PATH` that would **shadow host
  `libGL`/`libEGL`/`libstdc++`/`libwayland`/`libdrm`**. If `LD_LIBRARY_PATH` is needed, **re-exec**
  the process with it set (env changes don't retroactively apply). **Verify with `LD_DEBUG=libs`**
  that those host-coupled libs resolve to `/usr/...`, not our bundle.
- Nix platform (X11-EGL vs Wayland) is already handled in
  `libobs-wrapper/src/utils/initialization/other.rs` (`obs_set_nix_platform`).
- Gate the **bootstrapper out on Linux** (Cargo.toml + `context.rs`) per the earlier blocker analysis;
  the Linux provisioning above replaces it.

---

## 6. Host prerequisites + first-run preflight (support-burden reducer)

Cannot be bundled; must be present and **probed with precise per-item errors** (each missing piece
yields a different confusing symptom):
- GPU userspace driver (Mesa/NVIDIA GL+EGL) + `/dev/dri` — else swrast / no render.
- **Wayland capture:** PipeWire daemon **+** `xdg-desktop-portal` **+** a backend matching the
  compositor **+** D-Bus session bus — else black screen / "denied or cancelled".
- **HW encode (optional):** `libva` + a VA driver — else fall back.
- **Make HW encode strictly best-effort:** detect VAAPI at runtime (`vaInitialize` against host
  driver); on any failure, silently fall back to **bundled x264** (pure CPU, safe everywhere).

---

## 7. Consolidated failure modes → mitigations

| Failure mode | Mitigation |
|---|---|
| `GLIBC_2.xx not found` at dlopen | Build on oldest-glibc base; never bundle glibc. |
| swrast / `failed to load driver` (bundled `libstdc++` or `libGL`) | Don't bundle GL stack or libstdc++; if libstdc++ bundled, checkrt "newer-wins". Verify with `LD_DEBUG=libs`. |
| Deep dep `cannot open shared object` (RUNPATH non-transitive) | `$ORIGIN` RUNPATH on *every* object (or transitive `DT_RPATH`); + `LD_LIBRARY_PATH` set pre-exec. |
| VAAPI init fails / no driver | Prefer host libva; HW encode optional; fall back to x264. |
| `default.effect` not found / video init fails | Set `libobs_data_path` to bundled `data/libobs` (absolute). |
| Modules double-loaded / not found | Absolute normalized module paths; `%module%` data path; patch opengl-lib-name. |
| Black screen on Wayland | Probe + require host PipeWire daemon + portal + backend; clear error. |
| dlopen'd codec/driver missing from bundle | Manual include list + `strace openat` diff in CI. |

---

## 8. Testing matrix

- **Distros:** oldest+newest Ubuntu LTS, Fedora, Arch, an immutable (Silverblue/Bazzite), NixOS.
- **GPUs:** Intel (iHD), AMD (radeonsi), NVIDIA (proprietary).
- **Sessions:** X11; Wayland on GNOME, KDE, a wlroots compositor.
- **Assertions:** GL is hardware (not swrast — check `LD_DEBUG`/logs); VAAPI detect+fallback; PipeWire
  capture works; x264 fallback works; `strace openat` shows no unbundled `.so` misses.

---

## 9. Phased steps

0. **De-risking spike (do first):** extract an existing portable OBS (e.g. wimpysworld), override
   `StartupPaths` to it, patch `get_linux_opengl_lib_name`, and confirm libobs **inits + captures +
   encodes** on one distro. Proves the *consumption path* before investing in the build pipeline.
1. **Build pipeline:** minimal bundle in CI on oldest-glibc base; excludelist; `$ORIGIN`;
   strace-verified.
2. **Distribution:** host on `p-doom/libobs-builds` + checksum/signature.
3. **Runtime:** download → verify → atomic extract → load; version mgmt + lockfile + offline error.
4. **Fork patches:** `get_linux_opengl_lib_name` override; (optionally) Linux `StartupPaths` that
   accept a bundle root.
5. **Host preflight + precise error reporting.**
6. **Testing matrix.**

---

## Open decisions
- Build libobs **from source** (CMake, headless) vs **extract+trim** an existing portable build?
- glibc floor: Ubuntu 20.04 vs 22.04?
- Relocation policy: `$ORIGIN` RUNPATH everywhere vs transitive `DT_RPATH` vs scoped `LD_LIBRARY_PATH`.
- Bundle conservative `libva`/`libpipewire` as fallback, or strictly host? (Recommend host + x264 fallback.)
- Agent packaging (AppImage/.deb/tarball) is *separate* from this libobs bundle and can be decided later.

---

## Sources (selected, verified)
- AppImage excludelist (host-vs-bundle list + reasons): https://github.com/AppImage/pkg2appimage/blob/master/excludelist
- AppImage best-practices (build on oldest glibc/libstdc++): https://docs.appimage.org/reference/best-practices.html
- libstdc++/Mesa swrast clash: https://github.com/AppImage/AppImageKit/issues/1198 ; https://discourse.appimage.org/t/im-a-big-fan-of-this-but-graphics-driver-libstdc-conflict/171
- linuxdeploy-plugin-checkrt (newer-wins libstdc++): https://github.com/darealshinji/linuxdeploy-plugin-checkrt
- dlopen search order / RUNPATH non-transitive: https://man7.org/linux/man-pages/man3/dlopen.3.html
- linuxdeploy misses dlopen'd libs: https://docs.appimage.org/packaging-guide/from-source/linuxdeploy-user-guide.html
- OBS module API (`obs_add_module_path` / `%module%`): https://docs.obsproject.com/reference-modules
- OBS absolute-path module load fix: https://github.com/obsproject/obs-studio/pull/12042
- libobs effect-file data dir requirement: https://obsproject.com/forum/threads/obs_add_data_path-deprecation.190133/
- OBS Linux GL platform (EGL via gl-nix.c): https://github.com/obsproject/obs-studio/blob/master/libobs-opengl/gl-nix.c
- libGL/glvnd must be host (libcapsule discussion): https://github.com/NixOS/nixpkgs/issues/31189
- VAAPI loader/ABI: https://github.com/intel/libva ; https://wiki.debian.org/HardwareVideoAcceleration
- OBS Flatpak (GL/VAAPI runtime extensions + finish-args, bundles only app userspace): https://github.com/flathub/com.obsproject.Studio ; https://github.com/obsproject/rfcs/pull/21 ; https://docs.flatpak.org/en/latest/extension.html
- PipeWire ABI/compat: https://github.com/PipeWire/pipewire/blob/master/NEWS
- Reference portable OBS builds: https://github.com/wimpysworld/obs-studio-portable ; https://github.com/castrojo/obs-studio-portable-1
