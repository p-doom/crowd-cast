# Relocatable libobs bundle builder — AlmaLinux 9 (glibc 2.34).
# Chosen over EL8 because EL9 ships PipeWire >=0.3.47 and GLib 2.68 (OBS 32 needs PipeWire
# >=0.3.33 + a modern GLib for the portal path) without source-building them, while keeping
# a low, broad glibc floor (2.34 covers RHEL9, Ubuntu 22.04+, Debian 12+, Fedora, Arch).
# We build with the DEFAULT gcc (11) so the libstdc++/GLIBCXX requirement stays aligned with
# the 2.34 floor (a newer gcc-toolset would make binaries need a newer libstdc++ than
# 2.34-era hosts ship). BUILD-TIME dev headers for host-coupled libs (linked, not shipped).
FROM almalinux:9

RUN dnf -y install 'dnf-command(config-manager)' epel-release \
 && dnf config-manager --set-enabled crb \
 && dnf -y groupinstall "Development Tools" \
 && dnf -y install \
        git wget xz zstd file which patch pkgconfig nasm yasm \
        ninja-build patchelf extra-cmake-modules \
        pipewire-devel glib2-devel libdrm-devel libva-devel \
        mesa-libEGL-devel mesa-libGL-devel mesa-libgbm-devel \
        wayland-devel wayland-protocols-devel \
        libX11-devel libxcb-devel libXcomposite-devel libXfixes-devel \
        libXrandr-devel libXinerama-devel libXi-devel libxkbcommon-devel \
        pulseaudio-libs-devel alsa-lib-devel \
        jansson-devel libcurl-devel openssl-devel zlib-devel libuuid-devel \
 && dnf clean all

# CMake >= 3.28 (EL9 ships 3.26). Official static Kitware binary (old-glibc-safe).
RUN curl -fsSL https://github.com/Kitware/CMake/releases/download/v3.28.3/cmake-3.28.3-linux-x86_64.tar.gz \
      | tar xz -C /opt \
 && ln -sf /opt/cmake-3.28.3-linux-x86_64/bin/* /usr/local/bin/ \
 && cmake --version && ninja --version && patchelf --version

# GLib build deps. OBS 32 linux-pipewire needs Gio >=2.76 (for the static-inline g_clear_fd);
# EL9 ships 2.68, so we build GLib from source for build-time headers/linking (runtime uses
# host GLib). meson via pip (recent); symlink `ninja` since EPEL ships it as `ninja-build`.
RUN dnf -y install python3-pip libffi-devel pcre2-devel gettext-devel && dnf clean all \
 && python3 -m pip install --no-cache-dir meson \
 && ln -sf "$(command -v ninja-build)" /usr/local/bin/ninja \
 && meson --version && ninja --version

# libpci (obs-ffmpeg VAAPI GPU detection).
RUN dnf -y install pciutils-devel && dnf clean all

# MbedTLS: obs-outputs (which provides the mp4_output / mov_output Hybrid recording outputs)
# links it for its bundled librtmp. EL9 EPEL ships mbedtls 2.28 (LTS).
RUN dnf -y install mbedtls-devel && dnf clean all

WORKDIR /build
ENTRYPOINT ["/bin/bash"]
