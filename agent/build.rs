// Build script for compiling dmikushin/tray C library
//
// Platform-specific compilation:
// - macOS: Objective-C with Cocoa framework
// - Windows: C with shell32
// - Linux: C++ with Qt6 (handled separately)

fn main() {
    // Tell cargo about our custom cfg
    println!("cargo::rustc-check-cfg=cfg(no_tray)");
    // Only compile on supported platforms
    #[cfg(target_os = "macos")]
    compile_macos();

    #[cfg(target_os = "windows")]
    compile_windows();

    #[cfg(target_os = "linux")]
    compile_linux();

    // Tell cargo to rerun if tray sources change
    println!("cargo:rerun-if-changed=deps/tray/tray.h");
    println!("cargo:rerun-if-changed=deps/tray/tray_darwin.m");
    println!("cargo:rerun-if-changed=deps/tray/tray_windows.c");
    println!("cargo:rerun-if-changed=deps/tray/tray_linux.cpp");
    println!("cargo:rerun-if-changed=build.rs");
}

#[cfg(target_os = "macos")]
fn compile_macos() {
    cc::Build::new()
        .file("deps/tray/tray_darwin.m")
        .include("deps/tray")
        .flag("-fobjc-arc")
        .compile("tray");

    // Link Cocoa framework
    println!("cargo:rustc-link-lib=framework=Cocoa");
}

#[cfg(target_os = "windows")]
fn compile_windows() {
    cc::Build::new()
        .file("deps/tray/tray_windows.c")
        .include("deps/tray")
        .define("TRAY_EXPORTS", None)
        .compile("tray");

    // Link Windows libraries
    println!("cargo:rustc-link-lib=shell32");
    println!("cargo:rustc-link-lib=user32");
}

#[cfg(target_os = "linux")]
fn compile_linux() {
    // Linux uses Qt6 which requires C++ and pkg-config
    // For now, we'll use a simplified approach with appindicator if available
    // Full Qt6 support would require more complex build setup
    
    // Try to find appindicator3
    if pkg_config::probe_library("appindicator3-0.1").is_ok() {
        cc::Build::new()
            .file("deps/tray/tray_linux.cpp")
            .include("deps/tray")
            .cpp(true)
            .flag("-std=c++17")
            .compile("tray");
        
        println!("cargo:rustc-link-lib=appindicator3");
    } else if pkg_config::probe_library("ayatana-appindicator3-0.1").is_ok() {
        cc::Build::new()
            .file("deps/tray/tray_linux.cpp")
            .include("deps/tray")
            .cpp(true)
            .flag("-std=c++17")
            .compile("tray");
        
        println!("cargo:rustc-link-lib=ayatana-appindicator3");
    } else {
        // Fallback: compile without tray support on Linux
        // The Rust code will handle this gracefully
        println!("cargo:warning=No appindicator library found, tray support disabled on Linux");
        println!("cargo:rustc-cfg=no_tray");
    }
}
