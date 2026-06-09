#!/usr/bin/env bash
# Build a relocatable libobs bundle from pinned OBS source. Runs INSIDE the AlmaLinux 8
# container. Idempotent: x264/FFmpeg are skipped if already built in the cached /build/stage,
# clones are reused, OBS rebuilds incrementally. Output -> /out (host-mounted).
set -euo pipefail

# Modern toolchain (gcc-toolset-13). NOTE on libstdc++: binaries built here need GCC13-level
# GLIBCXX. We do NOT bundle libstdc++ yet (the build/test host, Manjaro, has a newer one, so it
# resolves). Cross-distro to OLD-libstdc++ hosts needs the "checkrt newer-wins" launcher shim
# (docs/LINUX_LIBOBS_PROVISIONING.md §1) — tracked as a follow-up.
source /opt/rh/gcc-toolset-13/enable 2>/dev/null || true
echo "::: toolchain: $(gcc --version | head -1)"

OBS_TAG="${OBS_TAG:-32.0.2}"
FFMPEG_TAG="${FFMPEG_TAG:-n7.1}"
X264_BR="${X264_BR:-stable}"
JOBS="${JOBS:-$(nproc)}"
SRC=/build/src; STAGE=/build/stage; BUNDLE=/build/bundle/usr; OUT=/out
mkdir -p "$SRC" "$STAGE" "$BUNDLE/lib/obs-plugins" "$BUNDLE/share/obs" "$OUT"
export PKG_CONFIG_PATH="$STAGE/lib/pkgconfig:$STAGE/lib64/pkgconfig:${PKG_CONFIG_PATH:-}"
export LD_LIBRARY_PATH="$STAGE/lib:$STAGE/lib64:${LD_LIBRARY_PATH:-}"

if ! ls "$STAGE"/lib*/libx264.so* >/dev/null 2>&1; then
  echo "::: building x264 ($X264_BR)"
  [ -d "$SRC/x264" ] || git clone --depth 1 -b "$X264_BR" https://code.videolan.org/videolan/x264.git "$SRC/x264"
  cd "$SRC/x264" && ./configure --prefix="$STAGE" --enable-shared --disable-cli --enable-pic
  make -j"$JOBS" && make install
else echo "::: x264 already built (skip)"; fi

if ! ls "$STAGE"/lib*/libavcodec.so* >/dev/null 2>&1; then
  echo "::: building FFmpeg ($FFMPEG_TAG)"
  [ -d "$SRC/ffmpeg" ] || git clone --depth 1 -b "$FFMPEG_TAG" https://github.com/FFmpeg/FFmpeg.git "$SRC/ffmpeg"
  cd "$SRC/ffmpeg" && ./configure --prefix="$STAGE" --enable-shared --disable-static \
      --disable-programs --disable-doc --enable-gpl --enable-libx264 --enable-vaapi --enable-pic
  make -j"$JOBS" && make install
else echo "::: FFmpeg already built (skip)"; fi

# SIMDe (header-only): libobs requires it; OBS official builds get it from their prebuilt
# deps bundle. We vendor it into the stage include dir so find_package(SIMDe) resolves.
if [ ! -d "$STAGE/include/simde" ]; then
  echo "::: fetching SIMDe headers"
  [ -d "$SRC/simde" ] || git clone --depth 1 https://github.com/simd-everywhere/simde.git "$SRC/simde"
  mkdir -p "$STAGE/include"
  cp -r "$SRC/simde/simde" "$STAGE/include/"
else echo "::: SIMDe already vendored (skip)"; fi

# uthash (header-only): also a libobs requirement from OBS's prebuilt deps bundle.
if [ ! -f "$STAGE/include/uthash.h" ]; then
  echo "::: fetching uthash headers"
  [ -d "$SRC/uthash" ] || git clone --depth 1 https://github.com/troydhanson/uthash.git "$SRC/uthash"
  mkdir -p "$STAGE/include"
  cp "$SRC/uthash/src/"*.h "$STAGE/include/"
else echo "::: uthash already vendored (skip)"; fi

# GLib >=2.76: OBS 32 linux-pipewire requires Gio 2.76 (for g_clear_fd, a static-inline).
# EL9 ships 2.68, so build GLib for build-time headers/linking. NOT bundled -- at runtime the
# plugin uses host GLib (>=2.68 is universal); g_clear_fd inlines, so no 2.76 runtime symbol.
if ! ls "$STAGE"/lib*/libgio-2.0.so* >/dev/null 2>&1; then
  echo "::: building GLib (glib-2-78 branch)"
  [ -d "$SRC/glib" ] || git clone --depth 1 -b glib-2-78 https://gitlab.gnome.org/GNOME/glib.git "$SRC/glib"
  cd "$SRC/glib"
  rm -rf _build
  meson setup _build --prefix="$STAGE" --buildtype=release --default-library=shared \
    -Dtests=false -Dlibmount=disabled -Dselinux=disabled -Dnls=disabled \
    -Dman=false -Dgtk_doc=false -Dlibelf=disabled
  meson compile -C _build && meson install -C _build
else echo "::: GLib already built (skip)"; fi

echo "::: OBS Studio $OBS_TAG"
[ -d "$SRC/obs-studio" ] || git clone --recursive --depth 1 -b "$OBS_TAG" https://github.com/obsproject/obs-studio.git "$SRC/obs-studio"
cd "$SRC/obs-studio"
# OBS 32 builds a fixed Linux plugin set with no per-plugin ENABLE flags; the defaults pull
# hard deps we don't want (v4l2, qsv11/libvpl, nvenc, webrtc/libdatachannel, outputs/mbedtls,
# vst, aja, decklink, browser/CEF, websocket). Overwrite the plugin list with our minimal set.
# (Note: OBS 32 merged obs-x264 into obs-ffmpeg, so x264 software encoding comes from there.)
cat > plugins/CMakeLists.txt <<'PLUGINS'
cmake_minimum_required(VERSION 3.28...3.30)
option(ENABLE_PLUGINS "Enable building OBS plugins" ON)
if(NOT ENABLE_PLUGINS)
  set_property(GLOBAL APPEND PROPERTY OBS_FEATURES_DISABLED "Plugin Support")
  return()
endif()
set_property(GLOBAL APPEND PROPERTY OBS_FEATURES_ENABLED "Plugin Support")
add_obs_plugin(linux-capture PLATFORMS LINUX FREEBSD OPENBSD)
add_obs_plugin(linux-pipewire PLATFORMS LINUX FREEBSD OPENBSD)
add_obs_plugin(linux-pulseaudio PLATFORMS LINUX FREEBSD OPENBSD)
add_obs_plugin(obs-ffmpeg)
add_obs_plugin(obs-outputs)
PLUGINS
cmake -S . -B build -G Ninja \
  -DCMAKE_BUILD_TYPE=RelWithDebInfo -DCMAKE_INSTALL_PREFIX="$STAGE" -DCMAKE_PREFIX_PATH="$STAGE" \
  -DSIMDe_INCLUDE_DIR="$STAGE/include" -DUthash_INCLUDE_DIR="$STAGE/include" \
  -DENABLE_UI=OFF -DENABLE_FRONTEND=OFF -DENABLE_BROWSER=OFF -DENABLE_VLC=OFF -DENABLE_VST=OFF \
  -DENABLE_AJA=OFF -DENABLE_DECKLINK=OFF -DENABLE_WEBSOCKET=OFF -DENABLE_SCRIPTING=OFF \
  -DENABLE_VIRTUALCAM=OFF -DENABLE_PIPEWIRE=ON -DENABLE_PULSEAUDIO=ON -DENABLE_ALSA=ON -DENABLE_WAYLAND=ON \
  -DENABLE_NEW_MPEGTS_OUTPUT=OFF
cmake --build build -j"$JOBS"
cmake --install build

echo "::: assembling relocatable bundle tree"
findlib() { find "$STAGE" -name "$1" -print 2>/dev/null | head -1; }
OBSLIB_DIR="$(dirname "$(findlib 'libobs.so*')")"
echo "libobs dir: $OBSLIB_DIR"
cp -av "$OBSLIB_DIR"/libobs.so*         "$BUNDLE/lib/"
cp -av "$OBSLIB_DIR"/libobs-opengl.so*  "$BUNDLE/lib/" 2>/dev/null || true
# bundled leaf media libs (FFmpeg + x264) — NOT host-coupled
for pat in 'libav*.so*' 'libsw*.so*' 'libpostproc.so*' 'libx264.so*'; do
  for sd in "$STAGE/lib" "$STAGE/lib64"; do cp -av "$sd"/$pat "$BUNDLE/lib/" 2>/dev/null || true; done
done
# libpci (pciutils): obs-ffmpeg's VAAPI encoder needs it for GPU detection. Leaf util lib
# (reads /sys); bundle so the plugin loads even on hosts without pciutils installed.
cp -av /usr/lib64/libpci.so* "$BUNDLE/lib/" 2>/dev/null || cp -av /usr/lib/libpci.so* "$BUNDLE/lib/" 2>/dev/null || true
# mbedtls (obs-outputs/librtmp): leaf crypto libs, safe to bundle so obs-outputs.so loads.
cp -av /usr/lib64/libmbed*.so* "$BUNDLE/lib/" 2>/dev/null || cp -av /usr/lib/libmbed*.so* "$BUNDLE/lib/" 2>/dev/null || true

PLUGSRC=""
for c in "$OBSLIB_DIR/obs-plugins" "$STAGE/lib64/obs-plugins" "$STAGE/lib/obs-plugins"; do
  [ -d "$c" ] && PLUGSRC="$c" && break; done
echo "plugin dir: $PLUGSRC"
for p in linux-pipewire linux-capture linux-pulseaudio obs-ffmpeg obs-outputs; do
  cp -av "$PLUGSRC/$p.so" "$BUNDLE/lib/obs-plugins/" 2>/dev/null || echo "WARN: $p.so not found"
done

# data dirs (libobs effects + per-plugin data)
DATA_OBS="$(find "$STAGE" -type d -path '*/data/libobs' -o -type d -path '*/share/obs/libobs' 2>/dev/null | head -1)"
[ -n "$DATA_OBS" ] && cp -a "$DATA_OBS" "$BUNDLE/share/obs/libobs"
mkdir -p "$BUNDLE/share/obs/obs-plugins"
DATA_PLUG="$(find "$STAGE" -type d -path '*/data/obs-plugins' -o -type d -path '*/share/obs/obs-plugins' 2>/dev/null | head -1)"
[ -n "$DATA_PLUG" ] && cp -a "$DATA_PLUG/." "$BUNDLE/share/obs/obs-plugins/"

echo "::: relocate (RUNPATH=\$ORIGIN; non-transitive -> stamp every object)"
find "$BUNDLE/lib" -maxdepth 1 -name '*.so*' -type f -exec patchelf --set-rpath '$ORIGIN' {} \; 2>/dev/null || true
find "$BUNDLE/lib/obs-plugins" -name '*.so' -type f -exec patchelf --set-rpath '$ORIGIN:$ORIGIN/..' {} \; 2>/dev/null || true

echo "::: SMOKE GATE 1 — mp4_output (obs-outputs) + ffmpeg_muxer (obs-ffmpeg) must be registered"
# grep -a directly on the file (NOT `strings | grep -q`): under `set -o pipefail`, grep -q
# closes the pipe on first match, strings gets SIGPIPE (141), and the pipeline reports
# failure even though the match succeeded -- a false negative. grep -a on a file avoids it.
grep -aq "mp4_output" "$BUNDLE/lib/obs-plugins/obs-outputs.so" \
  || { echo "FATAL: mp4_output absent from obs-outputs.so — bundle would repeat the recording bug"; exit 3; }
grep -aq "ffmpeg_muxer" "$BUNDLE/lib/obs-plugins/obs-ffmpeg.so" \
  || { echo "FATAL: ffmpeg_muxer absent from obs-ffmpeg.so"; exit 3; }
echo "OK: mp4_output + ffmpeg_muxer present"

echo "::: max glibc symbol required by libobs (forward-compat audit)"
objdump -T "$BUNDLE/lib/"libobs.so* 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1 || true

echo "::: packaging"
ARCH="$(uname -m)"
TB="$OUT/obs-bundle-${OBS_TAG}-${ARCH}.tar.zst"
tar -C /build/bundle -caf "$TB" usr
( cd "$OUT" && sha256sum "$(basename "$TB")" > "$(basename "$TB").sha256" )
echo "DONE: $TB"; ls -lh "$TB"*
