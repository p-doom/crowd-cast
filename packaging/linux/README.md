# Linux libobs bundle — production build

Builds a **reproducible, relocatable libobs bundle** (libobs + curated OBS plugins +
FFmpeg + x264) from **pinned OBS source**, so crowd-cast can `dlopen` it on any Linux
distro without the user installing OBS or having root. This replaces the ad-hoc
"extract a third-party portable build" approach (unowned provenance, missing features
like `mp4_output`).

## Why a container (and which one)

Portability comes from the **glibc floor**, not from where the build runs. We build in
**AlmaLinux 9 (glibc 2.34)**, so the binaries run on newer glibc distributions
(Ubuntu 22.04+, Debian 12+, RHEL 9, Fedora, Arch, ...). Building on the
host (Manjaro, bleeding-edge glibc) would stamp new `GLIBC_2.xx` requirements and fail
everywhere else. GitHub Actions would run this *same* container — so building locally
in Podman is production-equivalent; CI just automates + signs it later.

## ABI target

Pinned to **OBS Studio 32.0.2** to match `libobs-rs` `bindings_linux.rs`
(`LIBOBS_API_MAJOR/MINOR/PATCH = 32/0/2`). Changing the bindings ⇒ rebuild at the
matching OBS tag.

## What we bundle vs leave to the host

Short version: BUNDLE libobs + our plugins +
FFmpeg/x264 (+ output/TLS leaf libs). NEVER bundle glibc, libstdc++/libgcc, GL/EGL/glvnd/
Mesa DRI, libdrm, libva + VA drivers, libpipewire, libdbus, X11/Wayland client libs —
those are the host's (bundling them drops the host to swrast or breaks capture IPC).

## Run

    # one-time: install a container runtime (needs root)
    sudo pacman -S podman           # or: sudo pacman -S docker && enable it

    # build the bundle (runs the AlmaLinux 9 container, ~30-60 min first time)
    packaging/linux/run-build.sh

Output: `packaging/linux/out/obs-bundle-32.0.2-x86_64.tar.zst` + `.sha256`.

## Smoke test (the gate that would have caught the missing mp4_output)

- **In-build (no GPU needed):** asserts `mp4_output` + `ffmpeg_muxer` are registered in
  the built `obs-ffmpeg.so`, and that libobs loads + enumerates the output. Build fails
  if `mp4_output` is absent.
- **On host (real VAAPI):** `packaging/linux/smoke-test-host.sh <bundle-dir>` extracts the
  bundle and runs crowd-cast against it, asserting a recording file actually grows.
  Run this on the laptop (real Intel GPU); CI has no GPU so it only runs the software gate.
