//! The real CLI binary against a local release API: exit codes, JSON-only
//! stdout, silence without a terminal, and the update cache. No network.

#![cfg(target_os = "macos")]

use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> TestDir {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "umr-update-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        TestDir(dir)
    }

    fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }

    fn run(&self, api: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ultra-media-remote"))
            .args(args)
            .env("ULTRA_MEDIA_REMOTE_UPDATE_CACHE", self.cache())
            .env("ULTRA_MEDIA_REMOTE_UPDATE_API", api)
            .env_remove("ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK")
            .output()
            .unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Serves one release record to `count` requests.
fn serve(tag: &str, count: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/repos/o/r/releases/latest",
        listener.local_addr().unwrap()
    );
    let body = json!({"tag_name": tag, "draft": false, "prerelease": false, "body": "- Fixes"})
        .to_string();
    thread::spawn(move || {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0; 4096];
            let _ = stream.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    url
}

fn json_stdout(output: &Output) -> Value {
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.lines().count(), 1, "one JSON document: {text}");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn check_reports_available_and_current_with_exit_codes() {
    let dir = TestDir::new("check");
    let newer = dir.run(&serve("v99.0.0", 1), &["update", "--check"]);
    assert_eq!(newer.status.code(), Some(10));
    let report = json_stdout(&newer);
    assert_eq!(report["updateAvailable"], true);
    assert_eq!(report["latest"], "99.0.0");
    assert!(report["command"].as_str().unwrap().contains("install.sh"));
    assert!(newer.stderr.is_empty(), "stderr: {:?}", newer.stderr);

    let same = dir.run(
        &serve(&format!("v{}", env!("CARGO_PKG_VERSION")), 1),
        &["update", "--check"],
    );
    assert_eq!(same.status.code(), Some(0));
    assert_eq!(json_stdout(&same)["updateAvailable"], false);
}

#[test]
fn a_failed_check_exits_one_with_its_cause() {
    let dir = TestDir::new("fail");
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x", closed.local_addr().unwrap());
    drop(closed);
    let output = dir.run(&url, &["update", "--check"]);
    assert_eq!(output.status.code(), Some(1));
    let report = json_stdout(&output);
    assert_eq!(report["ok"], false);
    assert!(report["error"]
        .as_str()
        .unwrap()
        .contains("release check failed"));
}

#[test]
fn ordinary_commands_without_a_terminal_never_check_or_write() {
    let dir = TestDir::new("silent");
    // No server: a request would fail; none must be made and nothing written.
    for args in [&["caps"][..], &["--version"], &["--help"]] {
        let output = dir.run("http://127.0.0.1:9/never", args);
        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?} printed to stderr");
    }
    assert!(!dir.cache().exists(), "a piped command wrote update state");
}

#[test]
fn auto_and_notify_switches_persist_and_auto_off_cleans_up() {
    let dir = TestDir::new("switch");
    let api = "http://127.0.0.1:9/never";
    let status = json_stdout(&dir.run(api, &["update", "--auto", "status"]));
    assert_eq!(status["auto"], false, "auto is off by default");
    assert_eq!(status["notify"], true, "notices are on by default");
    assert_eq!(
        json_stdout(&dir.run(api, &["update", "--auto", "on"]))["auto"],
        true
    );
    assert_eq!(
        json_stdout(&dir.run(api, &["update", "--auto", "status"]))["auto"],
        true
    );
    let state = dir.cache().join("update.json");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o600
    );
    dir.run(api, &["update", "--auto", "off"]);
    assert!(!dir.cache().exists(), "auto off leaves no files");
    let bad = dir.run(api, &["update", "--auto", "maybe"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn update_refuses_a_copy_the_installer_did_not_make() {
    let dir = TestDir::new("cargo");
    let output = dir.run(&serve("v99.0.0", 1), &["update"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json_stdout(&output)["error"].as_str().unwrap().to_owned();
    assert!(
        error.contains("not installed with the installer"),
        "{error}"
    );
    assert!(
        error.contains("ULTRA_MEDIA_REMOTE_VERSION=99.0.0"),
        "{error}"
    );
}
