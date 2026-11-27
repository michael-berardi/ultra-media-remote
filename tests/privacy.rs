//! Offline regression checks for the privacy guarantees in README.md and
//! PRIVACY.md. The library sources are network-free; the CLI's update check
//! (src/bin) is the one documented exception.
//!
//! These tests only read repository files. They open no sockets, spawn no
//! processes and make no network calls.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

fn lowers(sources: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
    sources
        .iter()
        .map(|(path, text)| (*path, text.to_ascii_lowercase()))
        .collect()
}

fn assert_absent(sources: &[(&str, String)], needles: &[&str], context: &str) {
    for (path, text) in sources {
        for needle in needles {
            assert!(
                !text.contains(needle),
                "{context}: {needle:?} reappeared in {path}"
            );
        }
    }
}

fn manifest_and_lock() -> Vec<(&'static str, String)> {
    vec![
        ("Cargo.toml", read("Cargo.toml")),
        ("Cargo.lock", read("Cargo.lock")),
    ]
}

fn shipped_source() -> Vec<(&'static str, String)> {
    [
        "build.rs",
        "src/lib.rs",
        "native/UltraMediaRemote/Package.swift",
        "native/UltraMediaRemote/Sources/UltraMediaRemote/UltraMediaRemote.swift",
        "native/UltraMediaRemote/Sources/UltraMediaRemote/SystemAudioSpectrum.swift",
    ]
    .into_iter()
    .map(|path| (path, read(path)))
    .collect()
}

#[test]
fn no_telemetry_or_analytics_dependencies_are_declared() {
    let sources = lowers(&manifest_and_lock());
    let forbidden = [
        "sentry",
        "bugsnag",
        "posthog",
        "mixpanel",
        "amplitude",
        "datadog",
        "opentelemetry",
        "plausible",
        "segment",
        "appcenter",
        "firebase",
        "countly",
        "matomo",
        "hotjar",
        "umami",
        "telemetry",
        "analytics",
    ];
    assert_absent(&sources, &forbidden, "dependency privacy guard");
}

#[test]
fn shipped_code_has_no_telemetry_endpoints_or_sdk_references() {
    let sources = lowers(&shipped_source());
    let forbidden = [
        "sentry",
        "bugsnag",
        "posthog",
        "mixpanel",
        "@amplitude",
        "amplitude.com",
        "datadog",
        "opentelemetry",
        "plausible.io",
        "segment.io",
        "appcenter",
        "firebase",
        "googletagmanager",
        "google-analytics",
        "google analytics",
        "umami.io",
        "telemetry",
        "analytics",
    ];
    assert_absent(&sources, &forbidden, "source privacy guard");
}

#[test]
fn shipped_code_has_no_http_client_or_network_listener() {
    let mut sources = lowers(&manifest_and_lock());
    sources.extend(lowers(&shipped_source()));
    let forbidden = [
        "reqwest",
        "ureq",
        "hyper",
        "isahc",
        "tiny_http",
        "axum",
        "actix-web",
        "tungstenite",
        "std::net",
        "tcplistener",
        "tcpsocket",
        "udpsocket",
        "urlsession",
        "nsurlsession",
        "cfnetwork",
        "nwconnection",
        "nwlistener",
        "cfsocket",
        "libcurl",
        "listen(",
    ];
    assert_absent(&sources, &forbidden, "no-default-network guard");
}

#[test]
fn readme_has_no_remote_assets_or_web_ui_scripts() {
    let mut sources = lowers(&[("README.md", read("README.md"))]);
    sources.extend(lowers(&shipped_source()));
    let forbidden = [
        "img.shields.io",
        "cdn.jsdelivr.net",
        "unpkg.com",
        "fonts.googleapis.com",
        "fonts.gstatic.com",
        "cdnjs.cloudflare.com",
        "code.jquery.com",
        "<script",
        "@import url",
    ];
    assert_absent(&sources, &forbidden, "no-CDN guard");
}

#[test]
fn readme_privacy_is_short_and_privacy_md_holds_the_details() {
    let readme = read("README.md");
    let section = readme
        .split("\n## Privacy\n")
        .nth(1)
        .expect("README Privacy section is missing")
        .split("\n## ")
        .next()
        .unwrap();
    let lines = section.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        (3..=6).contains(&lines),
        "README Privacy must be 3-6 lines, found {lines}"
    );
    for phrase in ["No telemetry", "PRIVACY.md", "NO_UPDATE_CHECK"] {
        assert!(section.contains(phrase), "README Privacy lacks {phrase:?}");
    }
    let privacy = read("PRIVACY.md");
    for phrase in [
        "sends nothing over the network",
        "no telemetry",
        "stores nothing on disk",
        "rm -rf target native/UltraMediaRemote/.build-cargo",
        "ultra-media-remote-update-check",
        "ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK=1",
        "update --auto off",
        "256 KiB",
    ] {
        assert!(
            privacy.contains(phrase),
            "PRIVACY.md no longer states {phrase:?}"
        );
    }
}

#[test]
fn update_contract_docs_ship_with_the_marker() {
    let doc = read("docs/updates.md");
    assert!(doc.contains("<!-- update-contract: v1 -->"));
    assert!(doc.contains("The library crate has none of this"));
}

#[test]
fn version_is_aligned_across_manifest_lockfile_and_readme() {
    let manifest = read("Cargo.toml");
    let version_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("version = "))
        .expect("Cargo.toml has a package version");
    let version = version_line
        .split('"')
        .nth(1)
        .expect("package version is quoted");
    assert!(
        read("Cargo.lock").contains(&format!("version = \"{version}\"")),
        "Cargo.lock is not aligned with Cargo.toml"
    );
    assert!(
        read("README.md").contains(&format!("--tag v{version}")),
        "README install tag is not aligned with Cargo.toml"
    );
}
