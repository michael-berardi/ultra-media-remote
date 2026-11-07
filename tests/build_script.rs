// Compile the build script as a test module so its own regression tests run
// under `cargo test` without registering the same file as two Cargo targets.
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;
