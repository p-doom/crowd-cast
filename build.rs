//! Build script for crowd-cast agent
//!
//! On macOS, this sets up the necessary rpath for finding libobs at runtime
//! and compiles the tray icon C/Objective-C sources.

fn main() {
    configure_upload_endpoint();

    // Tell Cargo about the no_tray cfg
    println!("cargo::rustc-check-cfg=cfg(no_tray)");
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
        // On Windows, we need shell32 for the tray
        // For now, disable tray on Windows until we add the C sources
        println!("cargo:rustc-cfg=no_tray");
    }

    println!("cargo:rerun-if-changed=src/ui/tray.h");
    println!("cargo:rerun-if-changed=src/ui/tray_darwin.m");
    println!("cargo:rerun-if-changed=src/ui/notifications_darwin.m");
    println!("cargo:rerun-if-changed=src/ui/wizard_darwin.h");
    println!("cargo:rerun-if-changed=src/ui/wizard_darwin.m");
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
        panic!(
            "CROWD_CAST_API_GATEWAY_URL must start with http:// or https:// (got: {endpoint})"
        );
    }

    println!("cargo:rustc-env=CROWD_CAST_API_GATEWAY_URL={endpoint}");
}
