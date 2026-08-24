use std::env;
use std::path::PathBuf;
use std::process::Command;

/// Builds the native Swift package `UltraMediaRemote` and tells Cargo where to
/// find the resulting static library. On non-macOS hosts this is a no-op so
/// that `cargo check` can still validate the Rust code.
fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        println!("cargo:warning=ultra-media-remote only builds its Swift component on macOS");
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let swift_package_dir = manifest_dir.join("native").join("UltraMediaRemote");
    let build_profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let swift_build_config = if build_profile == "release" {
        "release"
    } else {
        "debug"
    };

    // The spectrum module compiles only when the `spectrum` cargo feature is
    // enabled, keeping default consumers free of ScreenCaptureKit code.
    let mut extra_args = Vec::new();
    if env::var_os("CARGO_FEATURE_SPECTRUM").is_some() {
        extra_args.push("-Xswiftc".to_string());
        extra_args.push("-DUMR_SPECTRUM".to_string());
    }

    println!(
        "cargo:rerun-if-changed={}",
        swift_package_dir.join("Package.swift").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        swift_package_dir.join("Sources").display()
    );

    let status = Command::new("swift")
        .arg("build")
        .arg("-c")
        .arg(&swift_build_config)
        .args(&extra_args)
        .current_dir(&swift_package_dir)
        .status()
        .expect("failed to run `swift build` for UltraMediaRemote");

    if !status.success() {
        panic!("`swift build` failed for UltraMediaRemote");
    }

    // SwiftPM places static libraries under .build/<config>.
    let lib_dir = swift_package_dir.join(".build").join(&swift_build_config);

    let lib_name = "libUltraMediaRemote.a";
    let lib_path = lib_dir.join(lib_name);
    if !lib_path.exists() {
        panic!(
            "Expected Swift static library at {}. Check Package.swift product type.",
            lib_path.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=UltraMediaRemote");
    // Swift's static archive carries the implementation, while Rust owns the
    // final link. Keep the public native frameworks explicit at that boundary.
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    if env::var_os("CARGO_FEATURE_SPECTRUM").is_some() {
        println!("cargo:rustc-link-lib=framework=CoreGraphics");
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
    }

    // On macOS 15+ the Swift concurrency runtime is provided by the system,
    // but the compiler still emits a reference to @rpath/libswift_Concurrency.dylib.
    // Add /usr/lib/swift as an rpath so the loader can resolve it from the dyld cache.
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}
