//! End-to-end tests for the one-line installer against a local release dir.
//!
//! By default a release dir is assembled in a temp folder from the CLI binary
//! Cargo just built plus fixture adapter parts. Set
//! `ULTRA_MEDIA_REMOTE_TEST_RELEASE_DIR` to a real `dist/release` directory to
//! run the same test against the notarized release assets.

#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ASSET: &str = "ultra-media-remote-macos-arm64.tar.gz";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "umr-installer-{tag}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(command: &mut Command) -> Output {
    command.output().expect("spawn command")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn make_executable(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// Returns a release dir with install.sh, the tarball and SHA256SUMS.
fn release_dir(temp: &TempDir) -> PathBuf {
    if let Some(dir) = std::env::var_os("ULTRA_MEDIA_REMOTE_TEST_RELEASE_DIR") {
        let dir = fs::canonicalize(PathBuf::from(dir)).expect("canonicalize release dir");
        assert!(
            dir.join("SHA256SUMS").is_file(),
            "the override has no SHA256SUMS"
        );
        return dir;
    }

    let root = temp.path().join("release");
    let app = root.join("ultra-media-remote.app/Contents");
    let adapter = app.join("Resources/mediaremote");
    fs::create_dir_all(app.join("MacOS")).expect("create app dir");
    fs::create_dir_all(adapter.join("MediaRemoteAdapter.framework")).expect("create adapter dir");

    let built = PathBuf::from(env!("CARGO_BIN_EXE_ultra-media-remote"));
    fs::copy(&built, app.join("MacOS/ultra-media-remote")).expect("copy CLI binary");
    make_executable(&app.join("MacOS/ultra-media-remote"));
    fs::write(root.join("VERSION"), env!("CARGO_PKG_VERSION")).expect("write VERSION");
    fs::write(
        adapter.join("mediaremote-adapter.pl"),
        "#!/bin/sh\nexit 0\n",
    )
    .expect("write adapter launcher");
    make_executable(&adapter.join("mediaremote-adapter.pl"));
    fs::write(
        adapter.join("MediaRemoteAdapter.framework/Info.plist"),
        "fixture framework\n",
    )
    .expect("write framework fixture");
    fs::copy(repo_root().join("LICENSE"), root.join("LICENSE")).expect("copy LICENSE");
    fs::copy(
        repo_root().join("THIRD_PARTY_NOTICES.md"),
        root.join("THIRD_PARTY_NOTICES.md"),
    )
    .expect("copy notices");

    let tarball = temp.path().join(ASSET);
    let output = run(Command::new("tar")
        .args(["-czf"])
        .arg(&tarball)
        .arg("-C")
        .arg(&root)
        .args([
            "ultra-media-remote.app",
            "VERSION",
            "LICENSE",
            "THIRD_PARTY_NOTICES.md",
        ]));
    assert!(
        output.status.success(),
        "tar: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::copy(
        repo_root().join("scripts/install.sh"),
        root.join("install.sh"),
    )
    .expect("copy installer");
    fs::copy(&tarball, root.join(ASSET)).expect("copy tarball into release dir");
    // Checksum lines are relative to the release dir, so the installer's
    // name-based lookup works for both the fixture and the real asset set.
    let sums = run(Command::new("shasum")
        .args(["-a", "256", ASSET, "install.sh"])
        .current_dir(&root));
    assert!(
        sums.status.success(),
        "shasum: {}",
        String::from_utf8_lossy(&sums.stderr)
    );
    fs::write(root.join("SHA256SUMS"), sums.stdout).expect("write SHA256SUMS");
    root
}

fn installer(release: &Path, home: &Path) -> Output {
    run(Command::new("sh")
        .arg(repo_root().join("scripts/install.sh"))
        .env("HOME", home)
        .env(
            "ULTRA_MEDIA_REMOTE_RELEASE_BASE",
            format!("file://{}", release.display()),
        )
        .env_remove("ULTRA_MEDIA_REMOTE_HOME")
        .env_remove("ULTRA_MEDIA_REMOTE_BIN_DIR")
        .env_remove("ULTRA_MEDIA_REMOTE_VERSION"))
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn installer_works_from_a_local_release_dir_with_a_temp_home() {
    let temp = TempDir::new("ok");
    let release = release_dir(&temp);
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).expect("set home mode");

    let output = installer(&release, &home);
    assert_success(&output, "installer");
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o755,
        "installer changed the mode of an existing home"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "stdout: {stdout}"
    );

    let shim = home.join(".local/bin/ultra-media-remote");
    assert!(shim.is_file(), "missing PATH shim");
    assert!(fs::metadata(&shim).unwrap().permissions().mode() & 0o111 != 0);
    let version = run(Command::new(&shim).arg("--version"));
    assert_success(&version, "shim --version");
    let version_text = String::from_utf8_lossy(&version.stdout);
    assert!(
        version_text.contains(env!("CARGO_PKG_VERSION")),
        "output: {version_text}"
    );

    let installed = home
        .join(".local/share/ultra-media-remote")
        .join(env!("CARGO_PKG_VERSION"));
    let contents = installed.join("ultra-media-remote.app/Contents");
    assert!(contents.join("MacOS/ultra-media-remote").is_file());
    assert!(contents
        .join("Resources/mediaremote/mediaremote-adapter.pl")
        .is_file());
    assert!(contents
        .join("Resources/mediaremote/MediaRemoteAdapter.framework")
        .is_dir());
    assert!(installed.join("LICENSE").is_file());
    assert!(installed.join("THIRD_PARTY_NOTICES.md").is_file());
    assert_eq!(
        fs::read_link(home.join(".local/share/ultra-media-remote/current")).unwrap(),
        installed
    );

    // Re-running over our own installation is safe and idempotent.
    let again = installer(&release, &home);
    assert_success(&again, "second installer run");
    assert!(shim.is_file());
}

#[test]
fn installer_refuses_a_foreign_shim() {
    let temp = TempDir::new("clobber");
    let release = release_dir(&temp);
    let home = temp.path().join("home");
    let bin = home.join(".local/bin");
    fs::create_dir_all(&bin).expect("create bin");
    let shim = bin.join("ultra-media-remote");
    fs::write(&shim, "#!/bin/sh\necho not-ours\n").expect("write foreign shim");
    make_executable(&shim);

    let output = installer(&release, &home);
    assert!(
        !output.status.success(),
        "installer replaced a foreign shim"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to replace"), "stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        "#!/bin/sh\necho not-ours\n"
    );
}

#[test]
fn installer_rejects_a_modified_archive() {
    let temp = TempDir::new("checksum");
    let release = release_dir(&temp);
    if std::env::var_os("ULTRA_MEDIA_REMOTE_TEST_RELEASE_DIR").is_some() {
        // Do not tamper with a real release directory used by another test run.
        return;
    }
    fs::write(release.join(ASSET), b"tampered archive\n").expect("tamper with archive");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");

    let output = installer(&release, &home);
    assert!(
        !output.status.success(),
        "installer accepted a corrupt archive"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("checksum mismatch"), "stderr: {stderr}");
    assert!(!home.join(".local/bin/ultra-media-remote").exists());
}

#[test]
fn installer_refuses_non_macos_hosts() {
    let temp = TempDir::new("platform");
    let fake_bin = temp.path().join("fake-bin");
    fs::create_dir_all(&fake_bin).expect("create fake bin");
    fs::write(fake_bin.join("uname"), "#!/bin/sh\necho Linux\n").expect("write uname stub");
    make_executable(&fake_bin.join("uname"));
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");

    let path = std::env::join_paths(
        std::iter::once(fake_bin.into_os_string()).chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|entry| entry.into_os_string()),
        ),
    )
    .expect("build PATH");
    let output = run(Command::new("sh")
        .arg(repo_root().join("scripts/install.sh"))
        .env("HOME", &home)
        .env("PATH", path));
    assert!(
        !output.status.success(),
        "installer accepted a non-macOS host"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("macOS binary only"), "stderr: {stderr}");
    assert!(!home.join(".local/bin/ultra-media-remote").exists());
}

/// A release dir for `version` whose binary is a stand-in script.
fn scripted_release(temp: &TempDir, version: &str) -> PathBuf {
    let root = temp.path().join(format!("release-{version}"));
    let stage = temp.path().join(format!("stage-{version}"));
    let app = stage.join("ultra-media-remote.app/Contents");
    let adapter = app.join("Resources/mediaremote");
    fs::create_dir_all(app.join("MacOS")).expect("create app dir");
    fs::create_dir_all(adapter.join("MediaRemoteAdapter.framework")).expect("create adapter");
    fs::create_dir_all(&root).expect("create release dir");
    let bin = app.join("MacOS/ultra-media-remote");
    fs::write(
        &bin,
        format!("#!/bin/sh\necho 'ultra-media-remote {version}'\n"),
    )
    .unwrap();
    make_executable(&bin);
    fs::write(adapter.join("mediaremote-adapter.pl"), "#!/bin/sh\n").unwrap();
    fs::write(stage.join("VERSION"), version).unwrap();
    fs::write(stage.join("LICENSE"), "MIT\n").unwrap();
    fs::write(stage.join("THIRD_PARTY_NOTICES.md"), "notices\n").unwrap();
    let tar = run(Command::new("tar")
        .arg("-czf")
        .arg(root.join(ASSET))
        .arg("-C")
        .arg(&stage)
        .args([
            "ultra-media-remote.app",
            "VERSION",
            "LICENSE",
            "THIRD_PARTY_NOTICES.md",
        ]));
    assert_success(&tar, "tar");
    let sums = run(Command::new("shasum")
        .args(["-a", "256", ASSET])
        .current_dir(&root));
    assert_success(&sums, "shasum");
    fs::write(root.join("SHA256SUMS"), sums.stdout).unwrap();
    root
}

#[test]
fn installer_keeps_one_earlier_release_and_the_shim_follows_current() {
    let temp = TempDir::new("prune");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let share = home.join(".local/share/ultra-media-remote");
    for version in ["9.0.0", "9.1.0", "9.2.0"] {
        let release = scripted_release(&temp, version);
        assert_success(&installer(&release, &home), version);
    }
    // A folder the installer did not make is never removed.
    let mut names: Vec<String> = fs::read_dir(&share)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["9.1.0", "9.2.0", "current"]);
    let shim = fs::read_to_string(home.join(".local/bin/ultra-media-remote")).unwrap();
    assert!(shim.contains("/current/ultra-media-remote.app/"), "{shim}");
    let version = run(Command::new(home.join(".local/bin/ultra-media-remote")).arg("--version"));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "ultra-media-remote 9.2.0"
    );

    fs::create_dir_all(share.join("1.0.0")).unwrap();
    let release = scripted_release(&temp, "9.3.0");
    assert_success(&installer(&release, &home), "9.3.0");
    assert!(share.join("1.0.0").is_dir(), "unmarked folder must be kept");
    assert!(!share.join("9.1.0").exists());
    assert!(share.join("9.2.0").is_dir());
}

#[test]
fn installer_handles_a_home_with_spaces_and_refuses_shell_metacharacters() {
    let temp = TempDir::new("spaces");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("create home");
    let spaced = temp.path().join("my apps/umr");
    for version in ["9.0.0", "9.1.0", "9.2.0"] {
        let release = scripted_release(&temp, version);
        let output = run(Command::new("sh")
            .arg(repo_root().join("scripts/install.sh"))
            .env("HOME", &home)
            .env("ULTRA_MEDIA_REMOTE_HOME", &spaced)
            .env(
                "ULTRA_MEDIA_REMOTE_RELEASE_BASE",
                format!("file://{}", release.display()),
            )
            .env_remove("ULTRA_MEDIA_REMOTE_BIN_DIR")
            .env_remove("ULTRA_MEDIA_REMOTE_VERSION"));
        assert_success(&output, version);
    }
    assert!(!spaced.join("9.0.0").exists(), "oldest release pruned");
    assert!(spaced.join("9.1.0").is_dir(), "rollback release kept");
    let version = run(Command::new(home.join(".local/bin/ultra-media-remote")).arg("--version"));
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "ultra-media-remote 9.2.0"
    );

    let release = scripted_release(&temp, "9.3.0");
    let output = run(Command::new("sh")
        .arg(repo_root().join("scripts/install.sh"))
        .env("HOME", &home)
        .env("ULTRA_MEDIA_REMOTE_HOME", temp.path().join("bad$name"))
        .env(
            "ULTRA_MEDIA_REMOTE_RELEASE_BASE",
            format!("file://{}", release.display()),
        )
        .env_remove("ULTRA_MEDIA_REMOTE_BIN_DIR"));
    assert!(!output.status.success(), "accepted a $ in the install path");
}
