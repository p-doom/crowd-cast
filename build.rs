//! Build script for crowd-cast agent
//!
//! On macOS, this sets up the necessary rpath for finding libobs at runtime
//! and compiles the tray icon C/Objective-C sources.

fn main() {
    configure_upload_endpoint();
    configure_google_oauth();
    configure_build_version();
    configure_updater();

    // OBS ABI this build's libobs bindings target. Baked so the runtime bundle path
    // (src/capture/context.rs) and the Linux RUNPATH below agree on which bundle dir to use.
    let obs_abi = resolve_obs_abi();
    println!("cargo:rerun-if-env-changed=CROWD_CAST_OBS_ABI");
    println!("cargo:rustc-env=CROWD_CAST_OBS_ABI={obs_abi}");

    // Tell Cargo about the no_tray cfg
    println!("cargo::rustc-check-cfg=cfg(no_tray)");
    println!("cargo::rustc-check-cfg=cfg(has_sparkle)");
    // macOS: Set rpath for finding libobs.framework and dylibs at runtime
    #[cfg(target_os = "macos")]
    {
        // Ensure OBS binaries are present during build on macOS.
        // Set CROWD_CAST_SKIP_OBS_INSTALL=1 to opt out.
        if std::env::var_os("CROWD_CAST_SKIP_OBS_INSTALL").is_none() {
            cargo_obs_build::install().expect(
                "Failed to install OBS binaries (set CROWD_CAST_SKIP_OBS_INSTALL=1 to skip)",
            );
        } else {
            println!(
                "cargo:warning=Skipping OBS binary install (CROWD_CAST_SKIP_OBS_INSTALL is set)"
            );
        }

        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/..");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/..");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks");

        // Build the tray Objective-C library
        cc::Build::new()
            .file("src/ui/tray_darwin.m")
            .flag("-fobjc-arc")
            .include("src/ui")
            .compile("tray");

        // Build the notifications Objective-C library
        cc::Build::new()
            .file("src/ui/notifications_darwin.m")
            .flag("-fobjc-arc")
            .include("src/ui")
            .compile("notifications_darwin");

        // Build the wizard Objective-C library
        cc::Build::new()
            .file("src/ui/wizard_darwin.m")
            .flag("-fobjc-arc")
            .include("src/ui")
            .compile("wizard_darwin");

        configure_sparkle();

        // Link frameworks
        println!("cargo:rustc-link-lib=framework=Cocoa");
        println!("cargo:rustc-link-lib=framework=UserNotifications");
        println!("cargo:rustc-link-lib=framework=ApplicationServices");
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
    }

    #[cfg(target_os = "linux")]
    {
        // Native GTK3 setup wizard (mirrors the macOS Cocoa wizard).
        let gtk = pkg_config::Config::new()
            .atleast_version("3.0.0")
            .probe("gtk+-3.0")
            .expect("gtk+-3.0 development files are required to build the Linux setup wizard");
        let mut wizard = cc::Build::new();
        wizard.file("src/ui/wizard_linux.c");
        for inc in &gtk.include_paths {
            wizard.include(inc);
        }
        wizard.compile("wizard_linux");
        println!("cargo:rerun-if-changed=src/ui/wizard_linux.c");

        // Tray: Linux uses the pure-Rust StatusNotifierItem tray (src/ui/tray_linux.rs via
        // the `ksni` crate), so `no_tray` is NOT set here — Linux runs the shared TrayApp
        // loop like macOS. The dmikushin/tray C library is macOS-only (see tray_ffi.rs).

        // RUNPATH so a binary installed at ~/.local/bin resolves the shipped libobs bundle
        // (~/.local/share/crowd-cast/obs/<abi>/usr/lib) WITHOUT LD_LIBRARY_PATH — this is what
        // lets us drop the interim run-crowd-cast.sh wrapper and makes the autostart .desktop
        // (Exec=<bare binary>) work. --enable-new-dtags emits DT_RUNPATH (searched AFTER
        // LD_LIBRARY_PATH), so dev runs that still set LD_LIBRARY_PATH are unaffected, and a
        // non-existent path (e.g. running from target/) is simply skipped by ld.so.
        println!("cargo:rustc-link-arg=-Wl,--enable-new-dtags");
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/../share/crowd-cast/obs/{obs_abi}/usr/lib"
        );
    }

    #[cfg(target_os = "windows")]
    {
        // The Windows tray is implemented in pure Rust via the tray-icon/muda
        // crates (see src/ui/tray_windows.rs), so no C sources or no_tray flag.

        // Embed the application manifest declaring Common Controls v6. Without
        // it, native-windows-gui's statically-imported comctl32 v6 functions
        // (SetWindowSubclass et al.) bind to comctl32 v5 and the process fails
        // to start with STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139).
        embed_resource::compile("resources/windows/crowd-cast.rc", embed_resource::NONE);
        println!("cargo:rerun-if-changed=resources/windows/crowd-cast.rc");
        println!("cargo:rerun-if-changed=resources/windows/crowd-cast.manifest");
        println!("cargo:rerun-if-changed=resources/windows/crowd-cast.ico");

        // Bake in the auto-update feed + signing key (empty -> updater is
        // unavailable at runtime, which is fine for dev/placeholder builds).
        configure_windows_updater();

        // Copy WinSparkle.dll next to the built exe so dev runs (and the
        // installer's source dir) find it beside the executable. Best-effort:
        // run scripts/fetch-winsparkle.ps1 to populate it; auto-update is
        // optional, so a missing DLL just disables it.
        const WINSPARKLE_DLL: &str = "build/winsparkle/0.9.3/WinSparkle.dll";
        println!("cargo:rerun-if-changed={WINSPARKLE_DLL}");
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            let dll = std::path::Path::new(WINSPARKLE_DLL);
            if dll.exists() {
                // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
                if let Some(target_dir) = std::path::Path::new(&out_dir).ancestors().nth(3) {
                    let _ = std::fs::copy(dll, target_dir.join("WinSparkle.dll"));
                }
            }
        }
    }

    println!("cargo:rerun-if-changed=src/ui/tray.h");
    println!("cargo:rerun-if-changed=src/ui/tray_darwin.m");
    println!("cargo:rerun-if-changed=src/ui/notifications_darwin.m");
    println!("cargo:rerun-if-changed=src/ui/updater_darwin.h");
    println!("cargo:rerun-if-changed=src/ui/updater_darwin.m");
    println!("cargo:rerun-if-changed=src/ui/wizard_darwin.h");
    println!("cargo:rerun-if-changed=src/ui/wizard_darwin.m");
}

// The version the Windows auto-updater compares (and that the appcast carries)
// is `{base}.{build_number}`, mirroring macOS's CFBundleShortVersionString +
// CFBundleVersion. The base defaults to the Cargo version; the build number is
// an auto-incrementing value supplied at release time (run number / timestamp).
// This is only emitted when a build number is set, so dev builds stay stable
// (and the updater falls back to CARGO_PKG_VERSION).
fn configure_build_version() {
    println!("cargo:rerun-if-env-changed=CROWD_CAST_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=CROWD_CAST_VERSION_BASE");
    if let Ok(num) = std::env::var("CROWD_CAST_BUILD_NUMBER") {
        let num = num.trim();
        if !num.is_empty() {
            let base = std::env::var("CROWD_CAST_VERSION_BASE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("CARGO_PKG_VERSION").ok())
                .unwrap_or_default();
            println!("cargo:rustc-env=CROWD_CAST_BUILD_VERSION={base}.{num}");
        }
    }
}

// OBS ABI (OBS major.minor.patch) this build's libobs-rs bindings target. Defaults to the
// pinned 32.0.2; overridable via CROWD_CAST_OBS_ABI when the bindings are bumped.
fn resolve_obs_abi() -> String {
    std::env::var("CROWD_CAST_OBS_ABI")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "32.0.2".to_string())
}

fn configure_google_oauth() {
    println!("cargo:rerun-if-env-changed=CROWD_CAST_GOOGLE_CLIENT_ID");
    println!("cargo:rerun-if-env-changed=CROWD_CAST_GOOGLE_CLIENT_SECRET");
    // Optional: OAuth is disabled if not set
    if let Ok(client_id) = std::env::var("CROWD_CAST_GOOGLE_CLIENT_ID") {
        let client_id = client_id.trim();
        if !client_id.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_GOOGLE_CLIENT_ID={client_id}");
        }
    }
    if let Ok(client_secret) = std::env::var("CROWD_CAST_GOOGLE_CLIENT_SECRET") {
        let client_secret = client_secret.trim();
        if !client_secret.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_GOOGLE_CLIENT_SECRET={client_secret}");
        }
    }
}

#[cfg(target_os = "windows")]
fn configure_windows_updater() {
    println!("cargo:rerun-if-env-changed=CROWD_CAST_APPCAST_URL");
    println!("cargo:rerun-if-env-changed=CROWD_CAST_ED_PUBLIC_KEY");
    if let Ok(url) = std::env::var("CROWD_CAST_APPCAST_URL") {
        let url = url.trim();
        if !url.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_APPCAST_URL={url}");
        }
    }
    if let Ok(key) = std::env::var("CROWD_CAST_ED_PUBLIC_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_ED_PUBLIC_KEY={key}");
        }
    }
}

// Optional in-app auto-update config, baked at build time and read via option_env! in
// src/ui/updater_linux.rs. If unset, the Linux updater stays inert (mirrors macOS treating a
// missing SUFeedURL as "auto-update unavailable"), so default builds are unaffected.
//   CROWD_CAST_UPDATE_FEED_URL: URL of the Ed25519-signed JSON manifest (manifest + manifest.sig).
//   CROWD_CAST_UPDATE_PUBKEY:   base64 of the 32-byte raw Ed25519 public key (== Sparkle SUPublicEDKey).
fn configure_updater() {
    println!("cargo:rerun-if-env-changed=CROWD_CAST_UPDATE_FEED_URL");
    println!("cargo:rerun-if-env-changed=CROWD_CAST_UPDATE_PUBKEY");
    if let Ok(url) = std::env::var("CROWD_CAST_UPDATE_FEED_URL") {
        let url = url.trim();
        if !url.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_UPDATE_FEED_URL={url}");
        }
    }
    if let Ok(key) = std::env::var("CROWD_CAST_UPDATE_PUBKEY") {
        let key = key.trim();
        if !key.is_empty() {
            println!("cargo:rustc-env=CROWD_CAST_UPDATE_PUBKEY={key}");
        }
    }

    // Monotonic build number (the release workflow passes github.run_number). Baked so the
    // updater can treat a same-marketing-version rebuild as "newer" and compare for newer-than
    // rather than mere inequality — the Linux analog of Sparkle's <sparkle:version> build number.
    // Defaults to 0 for dev builds (so a real release, build >= 1, always supersedes a dev build).
    println!("cargo:rerun-if-env-changed=CROWD_CAST_BUILD_NUMBER");
    let build_number = std::env::var("CROWD_CAST_BUILD_NUMBER")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0".to_string());
    println!("cargo:rustc-env=CROWD_CAST_BUILD_NUMBER={build_number}");
}

fn configure_upload_endpoint() {
    println!("cargo:rerun-if-env-changed=CROWD_CAST_API_GATEWAY_URL");

    let endpoint = std::env::var("CROWD_CAST_API_GATEWAY_URL").unwrap_or_else(|_| {
        panic!(
            "CROWD_CAST_API_GATEWAY_URL must be set at build time. Example:\n\
             CROWD_CAST_API_GATEWAY_URL='https://example.execute-api.us-east-1.amazonaws.com/prod/presign' cargo build --release"
        )
    });

    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        panic!("CROWD_CAST_API_GATEWAY_URL is set but empty");
    }

    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        panic!("CROWD_CAST_API_GATEWAY_URL must start with http:// or https:// (got: {endpoint})");
    }

    println!("cargo:rustc-env=CROWD_CAST_API_GATEWAY_URL={endpoint}");

    // Test-only flag: when set, the binary uploads clips to a segregated `uploads/TEST_VERSION/`
    // prefix instead of the real `uploads/<crate-version>/` (see src/upload/presigned.rs). Explicit
    // opt-in, NOT a fallback — prod builds leave it unset. A test build is announced loudly so it
    // can never be mistaken for a shippable one.
    println!("cargo:rerun-if-env-changed=CROWD_CAST_UPLOAD_TEST");
    // Enabled only on a NON-EMPTY value (matches the FEED_URL/PUBKEY handling above), so a workflow
    // that passes an empty string for prod releases does NOT accidentally route clips to TEST_VERSION.
    if std::env::var("CROWD_CAST_UPLOAD_TEST")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        println!("cargo:rustc-env=CROWD_CAST_UPLOAD_TEST=1");
        println!("cargo:warning=TEST BINARY: clips upload to uploads/TEST_VERSION/ — NOT a shippable build");
    }
}

#[cfg(target_os = "macos")]
fn configure_sparkle() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-env-changed=CROWD_CAST_SPARKLE_DIR");
    println!("cargo:rerun-if-env-changed=CROWD_CAST_SKIP_SPARKLE");

    if std::env::var_os("CROWD_CAST_SKIP_SPARKLE").is_some() {
        println!("cargo:warning=Skipping Sparkle integration (CROWD_CAST_SKIP_SPARKLE is set)");
        return;
    }

    const SPARKLE_VERSION: &str = "2.8.1";
    let sparkle_dir = std::env::var("CROWD_CAST_SPARKLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("build").join("sparkle").join(SPARKLE_VERSION));
    let sparkle_framework = sparkle_dir.join("Sparkle.framework");

    if !sparkle_framework.exists() {
        println!(
            "cargo:warning=Sparkle.framework not found at {}. Run scripts/fetch-sparkle.sh to enable auto-updates.",
            sparkle_framework.display()
        );
        return;
    }

    println!("cargo:rustc-cfg=has_sparkle");
    println!(
        "cargo:rustc-link-search=framework={}",
        sparkle_dir.to_string_lossy()
    );
    println!("cargo:rustc-link-lib=framework=Sparkle");

    cc::Build::new()
        .file("src/ui/updater_darwin.m")
        .flag("-fobjc-arc")
        .flag(&format!("-F{}", sparkle_dir.to_string_lossy()))
        .include("src/ui")
        .compile("updater_darwin");
}
