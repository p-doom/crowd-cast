//! Build script for crowd-cast agent
//!
//! On macOS, this sets up the necessary rpath for finding libobs at runtime
//! and compiles the tray icon C/Objective-C sources.

fn main() {
    configure_upload_endpoint();
    configure_google_oauth();

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
        // On Linux, we need GTK for the tray
        // For now, disable tray on Linux until we add the C sources
        println!("cargo:rustc-cfg=no_tray");
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
