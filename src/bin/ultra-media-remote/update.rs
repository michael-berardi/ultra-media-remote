//! Update notices and opt-in self-update for the CLI (update contract v1,
//! docs/updates.md). The library crate has none of this: it makes no network
//! requests and writes nothing; only this binary checks for releases.
//!
//! A copy from scripts/install.sh lives in `<home>/<x.y.z>/` with a
//! `<home>/current` link that the shim runs. `update` downloads the release
//! archive and `SHA256SUMS`, refuses the archive unless its checksum matches,
//! unpacks it, checks that it starts and reports the new version, moves it to
//! `<home>/<new>/` and switches `current` with one rename. The release it
//! replaced stays for `update --rollback` until the new one has run once (a
//! week at most). Any other copy (a local build, `cargo install`) is left alone
//! and gets the installer command to run instead.
//!
//! The only automatic network request is one anonymous GET of the public
//! latest-release record, at most once a day, from a detached helper after a
//! command finished on a terminal. State is one small 0600 file in the
//! platform cache folder.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const TOOL: &str = "ultra-media-remote";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const USER_AGENT: &str = "ultra-media-remote-update-check";
const ASSET: &str = "ultra-media-remote-macos-arm64.tar.gz";
/// Written by scripts/install.sh into every release folder it owns.
const MARKER: &str = ".ultra-media-remote-install";
/// The binary inside a release folder.
const BINARY: &str = "ultra-media-remote.app/Contents/MacOS/ultra-media-remote";
const DAY: u64 = 24 * 60 * 60;
const LOG_LIMIT: usize = 256 * 1024;
const WORK_PREFIX: &str = ".update-";
/// The replaced release goes once the new one has run, or after this long.
const ROLLBACK_KEEP: u64 = 7 * DAY;
/// Hidden command for the detached check and opted-in install.
pub(crate) const HELPER: &str = "__update-background";

/// Everything the updater remembers. Missing fields take their defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Unix time of the last check, successful or not.
    checked_at: Option<u64>,
    /// Newest stable release seen (`x.y.z`) and the start of its notes.
    latest: Option<String>,
    notes: Option<String>,
    /// Cause of the last failed check.
    error: Option<String>,
    /// Version whose notice is hidden until a newer one is released.
    skipped: Option<String>,
    /// Last notice shown, for the once-a-day-per-version limit.
    shown: Option<String>,
    shown_at: Option<u64>,
    /// Daily check and notice (`update --notify`). On by default.
    notify: bool,
    /// Automatic installs (`update --auto`). Off by default.
    auto: bool,
    /// The `[y/N]` question after an install is asked only once.
    auto_asked: bool,
    /// Version a background install last tried, and why it failed.
    auto_tried: Option<String>,
    auto_error: Option<String>,
    /// Version that ran before the last install, for `update --rollback`.
    previous: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            checked_at: None,
            latest: None,
            notes: None,
            error: None,
            skipped: None,
            shown: None,
            shown_at: None,
            notify: true,
            auto: false,
            auto_asked: false,
            auto_tried: None,
            auto_error: None,
            previous: None,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The update cache folder: `update.json` and the auto-install `update.log`.
struct Store {
    dir: PathBuf,
}

impl Store {
    /// `$ULTRA_MEDIA_REMOTE_UPDATE_CACHE`, else `~/Library/Caches/ultra-media-remote`.
    fn locate() -> Result<Store, String> {
        let var = |key: &str| {
            env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let dir = match var("ULTRA_MEDIA_REMOTE_UPDATE_CACHE") {
            Some(dir) => dir,
            None => {
                let home = var("HOME").ok_or("HOME is not set")?;
                if cfg!(target_os = "macos") {
                    home.join("Library/Caches").join(TOOL)
                } else {
                    var("XDG_CACHE_HOME")
                        .unwrap_or_else(|| home.join(".cache"))
                        .join(TOOL)
                }
            }
        };
        if !dir.is_absolute() {
            return Err(format!("update cache {} is not absolute", dir.display()));
        }
        Ok(Store { dir })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("update.json")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("update.log")
    }

    fn load(&self) -> State {
        fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
        write_private(&self.state_path(), &bytes)
    }

    /// Append a line, keeping the log under 256 KiB by dropping its oldest half.
    fn log(&self, line: &str) {
        let path = self.log_path();
        let mut bytes = fs::read(&path).unwrap_or_default();
        bytes.extend_from_slice(format!("{} {line}\n", now()).as_bytes());
        trim_log(&mut bytes);
        let _ = write_private(&path, &bytes);
    }

    /// `update --auto off`: delete the cache and the log. A file stays only to
    /// remember `--notify off`.
    fn forget(&self, state: &State) -> Result<(), String> {
        let _ = fs::remove_file(self.log_path());
        if !state.notify {
            return self.save(&State {
                notify: false,
                ..State::default()
            });
        }
        match fs::remove_file(self.state_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(format!("{}: {e}", self.state_path().display()))
            }
            _ => {}
        }
        let _ = fs::remove_dir(&self.dir); // only succeeds when nothing else is in it
        Ok(())
    }
}

fn trim_log(bytes: &mut Vec<u8>) {
    if bytes.len() > LOG_LIMIT {
        let cut = bytes.len() - LOG_LIMIT / 2;
        let start = bytes[cut..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| cut + i + 1);
        bytes.drain(..start);
    }
}

/// Atomic private write: a 0700 folder, a fresh 0600 temp file, one rename.
/// `create_new` refuses an existing file or symlink at the temp path.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let fail = |e: io::Error| format!("{}: {e}", path.display());
    let dir = path.parent().ok_or("update cache has no folder")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(fail)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map_err(fail)
}

/// Stable `x.y.z`, optionally `v`-prefixed. Anything else is not a version.
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.strip_prefix('v').unwrap_or(raw).split('.');
    let mut next = || {
        let part = parts.next()?;
        let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        (digits && (part == "0" || !part.starts_with('0')))
            .then(|| part.parse().ok())
            .flatten()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(
        (parse_version(candidate), parse_version(current)),
        (Some(new), Some(old)) if new > old
    )
}

/// `x.y.z` for a parseable version that differs from `current`.
fn install_target(version: &str, current: &str) -> Result<String, String> {
    match parse_version(version) {
        Some(v) if Some(v) != parse_version(current) => Ok(format!("{}.{}.{}", v.0, v.1, v.2)),
        _ => Err(format!("refusing to install version {version:?}")),
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    body: Option<String>,
}

/// The release as `(x.y.z, short notes)`; drafts, prereleases and odd tags are `None`.
fn parse_release(raw: &str) -> Result<Option<(String, String)>, String> {
    let release: Release =
        serde_json::from_str(raw).map_err(|e| format!("unreadable release record: {e}"))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    Ok(
        parse_version(&release.tag_name).map(|(major, minor, patch)| {
            (
                format!("{major}.{minor}.{patch}"),
                short_notes(release.body.as_deref().unwrap_or("")),
            )
        }),
    )
}

/// The first five lines of the release notes as plain text.
fn short_notes(body: &str) -> String {
    body.lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>']).trim())
        .filter(|line| !line.is_empty())
        .take(5)
        .map(|line| line.chars().take(100).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn github_slug() -> &'static str {
    REPOSITORY.trim_start_matches("https://github.com/")
}

fn api_url() -> String {
    env::var("ULTRA_MEDIA_REMOTE_UPDATE_API")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| {
            format!(
                "https://api.github.com/repos/{}/releases/latest",
                github_slug()
            )
        })
}

/// The base folder or URL of a release's files.
fn release_base(version: &str) -> String {
    env::var("ULTRA_MEDIA_REMOTE_RELEASE_BASE")
        .ok()
        .filter(|base| !base.is_empty())
        .map(|base| base.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{}/releases/download/v{version}",
                github_slug()
            )
        })
}

/// What a person can run for a copy this CLI cannot update itself: the Cargo
/// command for a `cargo install` copy, else the one-line installer.
fn install_hint(version: &str) -> String {
    let exe = env::current_exe().ok();
    hint_for(exe.as_deref(), version)
}

fn hint_for(exe: Option<&Path>, version: &str) -> String {
    let cargo = exe.is_some_and(|exe| {
        let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        exe.parent()
            .filter(|bin| bin.file_name() == Some("bin".as_ref()))
            .and_then(Path::parent)
            .is_some_and(|root| root.join(".crates2.json").is_file())
    });
    if cargo {
        format!("cargo install --git {REPOSITORY} --tag v{version} --locked --force {TOOL}")
    } else {
        format!(
            "curl -fsSL https://github.com/{}/releases/latest/download/install.sh | ULTRA_MEDIA_REMOTE_VERSION={version} sh",
            github_slug()
        )
    }
}

/// A release folder name: plain `x.y.z`, no `v` prefix (as the installer writes).
fn release_name(name: &str) -> bool {
    !name.starts_with('v') && parse_version(name).is_some()
}

/// curl with no `~/.curlrc`, no cookies and the fixed User-Agent.
fn curl() -> Command {
    let mut curl = Command::new("curl");
    curl.args([
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--location",
    ])
    .args(["--user-agent", USER_AGENT])
    .stdin(Stdio::null());
    curl
}

/// The daily check: one anonymous GET, 4 s at most.
fn fetch(url: &str) -> Result<String, String> {
    let output = curl()
        .args([
            "--max-redirs",
            "3",
            "--max-time",
            "4",
            "--max-filesize",
            "1000000",
        ])
        .args(if url.starts_with("https://") {
            &["--proto", "=https", "--proto-redir", "=https"][..]
        } else {
            &[][..]
        })
        .args(["--header", "Accept: application/vnd.github+json", url])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("release check failed: {}", tail(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| "release record is not UTF-8".into())
}

/// A release file from a URL or, for tests and mirrors, a local folder.
fn download(base: &str, name: &str, dest: &Path) -> Result<(), String> {
    if !["https://", "http://", "file://"]
        .iter()
        .any(|scheme| base.starts_with(scheme))
    {
        return fs::copy(Path::new(base).join(name), dest)
            .map(|_| ())
            .map_err(|e| format!("cannot copy {name} from {base}: {e}"));
    }
    let mut curl = curl();
    curl.args(["--max-redirs", "5", "--max-time", "300", "--retry", "2"]);
    if base.starts_with("https://") {
        curl.args(["--proto", "=https", "--proto-redir", "=https"]);
    }
    let output = curl
        .arg("--output")
        .arg(dest)
        .arg(format!("{base}/{name}"))
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| format!("download of {name} failed: {}", tail(&output.stderr)))
}

/// The SHA-256 that a `SHA256SUMS` file lists for `name`.
fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (hash, file) = (fields.next()?, fields.next()?);
        let hex = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (hex && file.trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

/// SHA-256 of a file through the system `shasum`, so the CLI needs no hashing crate.
fn sha256_file(path: &Path) -> Result<String, String> {
    let output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run shasum: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let hash = text.split_whitespace().next().unwrap_or_default();
    if !output.status.success() || hash.len() != 64 {
        return Err(format!(
            "cannot hash {}: {}",
            path.display(),
            tail(&output.stderr)
        ));
    }
    Ok(hash.to_ascii_lowercase())
}

/// The last lines of a tool's stderr, short enough for a JSON error.
fn tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" | ");
    tail.chars().take(400).collect()
}

/// Ask for the latest release now and record the result or the failure's cause.
fn check(store: &Store, url: &str) -> Result<State, String> {
    let fetched = fetch(url).and_then(|raw| parse_release(&raw));
    let mut state = store.load();
    state.checked_at = Some(now());
    match &fetched {
        Ok(found) => {
            state.error = None;
            (state.latest, state.notes) = found.clone().unzip();
        }
        Err(cause) => state.error = Some(cause.clone()),
    }
    store.save(&state)?;
    fetched.map(|_| state)
}

/// A clock set backwards does not stall checks or notices.
fn due(at: Option<u64>, now: u64) -> bool {
    at.is_none_or(|at| at > now || now - at >= DAY)
}

fn available<'a>(state: &'a State, current: &str) -> Option<&'a str> {
    state
        .latest
        .as_deref()
        .filter(|latest| is_newer(latest, current))
}

/// Automatic checks and notices are for a person at a terminal: never in CI,
/// agent shells or after an opt-out. Explicit `update` commands are not gated.
fn automatic(terminal: bool, state: &State, var: impl Fn(&str) -> Option<String>) -> bool {
    let set = |key: &str| {
        var(key).is_some_and(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
    };
    let off = [
        "ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK",
        "CI",
        "CLAUDECODE",
        "CODEX_SANDBOX",
        "GEMINI_CLI",
    ];
    terminal && state.notify && !off.into_iter().any(set)
}

fn notice(state: &State, current: &str, now: u64) -> Option<[String; 2]> {
    let new = available(state, current)?;
    let shown_today = state.shown.as_deref() == Some(new) && !due(state.shown_at, now);
    if state.skipped.as_deref() == Some(new) || shown_today {
        return None;
    }
    let skip = format!("skip: {TOOL} update --skip");
    Some([
        format!("{TOOL} {new} available (you have {current}) · run: {TOOL} update"),
        if state.auto {
            skip
        } else {
            format!("turn on automatic updates: {TOOL} update --auto on · {skip}")
        },
    ])
}

fn write_notice(out: &mut impl Write, lines: &[String; 2], color: bool) -> io::Result<()> {
    let (dim, reset) = if color {
        ("\x1b[2m", "\x1b[0m")
    } else {
        ("", "")
    };
    write!(out, "{}\n{dim}{}{reset}\n", lines[0], lines[1])
}

fn auto_pending(state: &State, current: &str) -> bool {
    state.auto
        && available(state, current).is_some_and(|new| {
            state.skipped.as_deref() != Some(new) && state.auto_tried.as_deref() != Some(new)
        })
}

/// After an ordinary command: at most one notice on stderr, then at most one
/// detached helper. Nothing here waits for the network.
pub(crate) fn after_command() {
    let terminal = io::stdout().is_terminal() && io::stderr().is_terminal();
    if !terminal {
        return;
    }
    let Ok(store) = Store::locate() else { return };
    let mut state = store.load();
    if !automatic(terminal, &state, |key| env::var(key).ok()) {
        return;
    }
    let now = now();
    if let Some(lines) = notice(&state, CURRENT, now) {
        let color =
            env::var_os("NO_COLOR").is_none() && env::var("TERM").map_or(true, |t| t != "dumb");
        if write_notice(&mut io::stderr(), &lines, color).is_ok() {
            state.shown = state.latest.clone();
            state.shown_at = Some(now);
            let _ = store.save(&state);
        }
    }
    let check = due(state.checked_at, now);
    if !check && !auto_pending(&state, CURRENT) {
        return;
    }
    if check {
        // Claim today's check first, so commands run side by side make one request.
        state.checked_at = Some(now);
        let _ = store.save(&state);
    }
    let exe = env::current_exe().unwrap_or_else(|_| TOOL.into());
    // Low priority through nice(1) when it is there; the helper never blocks a command.
    let mut helper = if Path::new("/usr/bin/nice").is_file() {
        let mut nice = Command::new("/usr/bin/nice");
        nice.args(["-n", "10"]).arg(exe);
        nice
    } else {
        Command::new(exe)
    };
    helper.arg(HELPER);
    if check {
        helper.arg("--check");
    }
    let spawned = helper
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(e) = spawned {
        state.error = Some(format!("cannot start the update check: {e}"));
        let _ = store.save(&state);
    }
}

/// The detached helper: the daily check when asked, then an opted-in install.
pub(crate) fn background(check_first: bool) {
    let Ok(store) = Store::locate() else { return };
    if check_first && check(&store, &api_url()).is_err() {
        return; // the cause is in the cache
    }
    if auto_pending(&store.load(), CURRENT) {
        auto_install(&store, Installer::locate(), CURRENT);
    }
}

/// One background attempt per version. A failure keeps the current version,
/// records its cause and leaves the notice to offer `update` again.
fn auto_install(store: &Store, installer: Result<Installer, String>, current: &str) {
    let mut state = store.load();
    if !auto_pending(&state, current) {
        return;
    }
    let new = state.latest.clone().unwrap_or_default();
    state.auto_tried = Some(new.clone());
    state.auto_error = None;
    if store.save(&state).is_err() {
        return; // no record, no attempt: never a retry loop
    }
    store.log(&format!("installing {new}"));
    let result = installer.and_then(|installer| installer.install(&new, current, false));
    let mut state = store.load();
    match result {
        Ok(()) => {
            state.previous = Some(current.into());
            store.log(&format!("installed {new}; it runs from the next command"));
        }
        Err(cause) => {
            store.log(&format!("install of {new} failed: {cause}"));
            state.auto_error = Some(cause);
        }
    }
    let _ = store.save(&state);
}

/// The scripts/install.sh home that owns `exe`:
/// `<home>/<x.y.z>/ultra-media-remote.app/Contents/MacOS/ultra-media-remote`,
/// with the installer's marker, while `<home>/current` points at that release.
fn installer_home(exe: &Path) -> Option<PathBuf> {
    let exe = fs::canonicalize(exe).ok()?;
    let release = exe.ancestors().nth(4)?;
    if !exe.ends_with(BINARY) || !release.join(MARKER).is_file() {
        return None;
    }
    parse_version(release.file_name()?.to_str()?)?;
    let home = release.parent()?;
    let current = fs::canonicalize(home.join("current")).ok()?;
    (current == release).then(|| home.to_path_buf())
}

/// Point `<home>/current` at `release` with one rename, so a command starting
/// at the same moment runs either the old release or the new one, never neither.
fn swap_current(home: &Path, release: &Path) -> Result<(), String> {
    let fresh = home.join(format!(".current.{}", std::process::id()));
    let _ = fs::remove_file(&fresh);
    symlink(release, &fresh)
        .and_then(|()| fs::rename(&fresh, home.join("current")))
        .map_err(|e| {
            let _ = fs::remove_file(&fresh);
            format!("cannot switch {}: {e}", home.join("current").display())
        })
}

/// Remove every installer release folder except the current one and `keep`.
/// Only `x.y.z` folders holding the installer's marker are touched.
fn prune_releases(home: &Path, keep: &[&Path]) {
    let current = fs::canonicalize(home.join("current")).ok();
    let Ok(entries) = fs::read_dir(home) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = entry.file_name().to_str().is_some_and(release_name)
            && entry.file_type().is_ok_and(|t| t.is_dir())
            && path.join(MARKER).is_file();
        if !ours {
            continue;
        }
        let real = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let kept = current.as_deref() == Some(&real)
            || keep
                .iter()
                .any(|k| fs::canonicalize(k).ok().as_deref() == Some(&real));
        if !kept {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// After an ordinary command of an installer copy: once the release in use has
/// run successfully, or a week after the switch, the release it replaced goes.
pub(crate) fn settle(succeeded: bool) {
    if let Ok(exe) = env::current_exe() {
        settle_at(&exe, succeeded, now());
    }
}

fn settle_at(exe: &Path, succeeded: bool, now: u64) {
    let Some(home) = installer_home(exe) else {
        return; // not an installer copy, or an older process after a switch
    };
    let switched = fs::symlink_metadata(home.join("current"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_secs());
    if succeeded || now.saturating_sub(switched) >= ROLLBACK_KEEP {
        prune_releases(&home, &[]);
    }
}

/// A private work folder inside the install home, removed on success,
/// failure and panic. Same filesystem as the releases, so the move is a rename.
struct WorkDir(PathBuf);

impl WorkDir {
    fn create(home: &Path) -> Result<WorkDir, String> {
        sweep_stale_work(home, now());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = home.join(format!("{WORK_PREFIX}{}-{nanos}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(WorkDir(dir))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An update killed mid-way leaves its folder; the next one removes any such
/// folder older than a day.
fn sweep_stale_work(home: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(home) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry.file_name().to_string_lossy().starts_with(WORK_PREFIX);
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |t| now.saturating_sub(t.as_secs()));
        if ours && age >= DAY {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// The Developer ID team that signed an app bundle, from `codesign -dv`.
fn team_id(app: &Path) -> Option<String> {
    let output = Command::new("codesign")
        .args(["-dv", "--verbose=2"])
        .arg(app)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| team.len() == 10 && team.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(str::to_owned)
}

/// Refuse an app bundle unless its signature is valid (strict, deep) and comes
/// from the same Developer ID team as the copy that is running.
fn verify_signature(app: &Path, team: &str) -> Result<(), String> {
    let requirement =
        format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
    let output = Command::new("codesign")
        .args(["--verify", "--strict", "--deep", "-R"])
        .arg(&requirement)
        .arg(app)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run codesign: {e}"))?;
    output.status.success().then_some(()).ok_or_else(|| {
        format!(
            "the downloaded app is not signed by team {team}: {}; nothing was installed",
            tail(&output.stderr)
        )
    })
}

/// One update at a time per install home: a lock folder made with one
/// `mkdir`, removed on drop. A lock left by a killed update for a day is stale.
struct Lock(PathBuf);

impl Lock {
    fn take(home: &Path) -> Result<Lock, String> {
        let path = home.join(".update-lock");
        for _ in 0..2 {
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Lock(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let age = fs::symlink_metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map_or(0, |d| d.as_secs());
                    if age < DAY {
                        return Err("another update of this copy is running".into());
                    }
                    let _ = fs::remove_dir(&path);
                }
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        Err(format!("cannot take the update lock {}", path.display()))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Replaces an installer copy with another release.
struct Installer {
    home: PathBuf,
    /// `ULTRA_MEDIA_REMOTE_RELEASE_BASE` or the GitHub release of the version.
    base: Box<dyn Fn(&str) -> String>,
    /// The Developer ID team a new release must be signed by; `None` only in tests.
    team: Option<String>,
}

impl Installer {
    fn locate() -> Result<Installer, String> {
        let exe = env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        let home = installer_home(&exe)
            .ok_or_else(|| format!("{} was not installed with the installer", exe.display()))?;
        let app = fs::canonicalize(&exe)
            .ok()
            .and_then(|exe| exe.ancestors().nth(3).map(Path::to_path_buf))
            .ok_or("cannot find this copy's app bundle")?;
        let team = team_id(&app).ok_or_else(|| {
            format!(
                "{} carries no Developer ID signature; reinstall with the installer",
                app.display()
            )
        })?;
        Ok(Installer {
            home,
            base: Box::new(release_base),
            team: Some(team),
        })
    }

    /// Download the archive and SHA256SUMS into a private folder, refuse the
    /// archive unless its checksum matches, unpack it, refuse it unless it is
    /// signed by the same Developer ID team as this copy, check that it is
    /// `version` and starts, move it to `<home>/<version>` and switch `current`.
    /// The release that was current stays as the one rollback copy.
    fn install(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let _lock = Lock::take(&self.home)?;
        self.install_locked(version, current, progress)
    }

    fn install_locked(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let version = install_target(version, current)?;
        let target = self.home.join(&version);
        let previous = fs::canonicalize(self.home.join("current")).ok();
        if previous.is_some() && previous == fs::canonicalize(&target).ok() {
            return Err(format!("{version} is already the current release"));
        }
        let base = (self.base)(&version);
        let work = WorkDir::create(&self.home)?;
        if progress {
            eprintln!("Downloading {TOOL} {version}...");
        }
        let (sums, archive) = (work.0.join("SHA256SUMS"), work.0.join(ASSET));
        download(&base, "SHA256SUMS", &sums)?;
        download(&base, ASSET, &archive)?;
        let listed = fs::read_to_string(&sums).map_err(|e| format!("SHA256SUMS: {e}"))?;
        let want = listed_sum(&listed, ASSET).ok_or(format!("no checksum for {ASSET}"))?;
        if sha256_file(&archive)? != want {
            return Err(format!(
                "checksum mismatch for {ASSET}; nothing was installed"
            ));
        }
        let payload = work.0.join("release");
        fs::create_dir(&payload).map_err(|e| format!("{}: {e}", payload.display()))?;
        let tar = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        if !tar.status.success() {
            return Err(format!("cannot unpack {ASSET}: {}", tail(&tar.stderr)));
        }
        let stated = fs::read_to_string(payload.join("VERSION")).unwrap_or_default();
        if stated.trim() != version {
            return Err(format!("{ASSET} is not release {version}"));
        }
        if let Some(team) = &self.team {
            verify_signature(&payload.join("ultra-media-remote.app"), team)?;
        }
        let ran = Command::new(payload.join(BINARY))
            .arg("--version")
            .env("ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("the downloaded CLI does not run here: {e}"))?;
        let reported = String::from_utf8_lossy(&ran.stdout);
        if !ran.status.success() || reported.trim() != format!("{TOOL} {version}") {
            return Err(format!("the downloaded CLI does not report {version}"));
        }
        fs::write(
            payload.join(MARKER),
            format!("installer={ASSET}\nversion={version}\n"),
        )
        .map_err(|e| format!("cannot mark the release: {e}"))?;
        if target.exists() {
            if !target.join(MARKER).is_file() {
                return Err(format!(
                    "{} exists but was not installed by the installer; refusing to replace it",
                    target.display()
                ));
            }
            fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        fs::rename(&payload, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        swap_current(&self.home, &target)?;
        prune_releases(
            &self.home,
            &previous.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        );
        Ok(())
    }

    /// `update --rollback`: switch back to the kept folder when it is still
    /// there, else install that release again.
    fn rollback(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let _lock = Lock::take(&self.home)?;
        let version = install_target(version, current)?;
        let kept = self.home.join(&version);
        let stated = fs::read_to_string(kept.join("VERSION")).unwrap_or_default();
        if stated.trim() != version || !kept.join(MARKER).is_file() || !kept.join(BINARY).is_file()
        {
            return self.install_locked(&version, current, progress);
        }
        let left = fs::canonicalize(self.home.join("current")).ok();
        swap_current(&self.home, &kept)?;
        prune_releases(
            &self.home,
            &left.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        );
        Ok(())
    }

    /// The one kept release folder other than the current one, if there is
    /// exactly one: the rollback target when the state file was deleted.
    fn kept_release(&self) -> Option<String> {
        let current = fs::canonicalize(self.home.join("current")).ok();
        let mut kept = fs::read_dir(&self.home)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_str().is_some_and(release_name))
            .filter(|e| e.path().join(MARKER).is_file() && e.path().join(BINARY).is_file())
            .filter(|e| fs::canonicalize(e.path()).ok() != current)
            .filter_map(|e| e.file_name().into_string().ok());
        let first = kept.next()?;
        kept.next().is_none().then_some(first)
    }
}

fn confirm(question: &str, default: bool) -> bool {
    eprint!("{question} ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        Ok(0) | Err(_) => default,
        Ok(_) => match answer.trim().to_ascii_lowercase().as_str() {
            "" => default,
            answer => answer.starts_with('y'),
        },
    }
}

/// What `ultra-media-remote update` was asked to do.
#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    Install,
    Check,
    Skip,
    Auto(String),
    Notify(String),
    Rollback,
}

/// Parse the arguments after `update`.
pub(crate) fn parse_action(args: &[String]) -> Result<Action, String> {
    let switch = |value: Option<&String>, flag: &str| match value.map(String::as_str) {
        Some(v @ ("on" | "off" | "status")) => Ok(v.to_owned()),
        _ => Err(format!("'update {flag}' takes on, off or status")),
    };
    match args {
        [] => Ok(Action::Install),
        [flag] if flag == "--check" => Ok(Action::Check),
        [flag] if flag == "--skip" => Ok(Action::Skip),
        [flag] if flag == "--rollback" => Ok(Action::Rollback),
        [flag, rest @ ..] if flag == "--auto" && rest.len() <= 1 => {
            switch(rest.first(), flag).map(Action::Auto)
        }
        [flag, rest @ ..] if flag == "--notify" && rest.len() <= 1 => {
            switch(rest.first(), flag).map(Action::Notify)
        }
        _ => Err(
            "'update' takes one of --check, --skip, --rollback, --auto on|off|status, --notify on|off|status"
                .to_owned(),
        ),
    }
}

/// Run an `update` command; prints one JSON document on stdout.
pub(crate) fn command(action: Action) -> ExitCode {
    let terminal = io::stdin().is_terminal() && io::stderr().is_terminal();
    let result = Store::locate().and_then(|store| match action {
        Action::Install => {
            install_latest(&store, &api_url(), Installer::locate(), CURRENT, terminal)
        }
        Action::Check => {
            let managed = Installer::locate().is_ok();
            check_report(&store, &api_url(), CURRENT, managed)
        }
        Action::Skip => skip(&store, &api_url(), CURRENT),
        Action::Auto(value) => configure(&store, Some(&value), None),
        Action::Notify(value) => configure(&store, None, Some(&value)),
        Action::Rollback => rollback(&store, Installer::locate(), CURRENT, terminal),
    });
    match result {
        Ok((report, code)) => {
            println!("{report}");
            ExitCode::from(code)
        }
        Err(error) => {
            println!("{}", json!({"ok": false, "error": error}));
            ExitCode::from(1)
        }
    }
}

/// `update --check`: exit 0 up to date, 10 update available, 1 check failed.
fn check_report(
    store: &Store,
    url: &str,
    current: &str,
    managed: bool,
) -> Result<(Value, u8), String> {
    let state = check(store, url)?;
    let new = available(&state, current);
    let command = new.map(|new| {
        if managed {
            format!("{TOOL} update")
        } else {
            install_hint(new)
        }
    });
    let report = json!({
        "ok": true, "current": current, "latest": state.latest,
        "updateAvailable": new.is_some(), "notes": new.and(state.notes.as_ref()),
        "skipped": state.skipped, "command": command,
    });
    Ok((report, if new.is_some() { 10 } else { 0 }))
}

fn skip(store: &Store, url: &str, current: &str) -> Result<(Value, u8), String> {
    let mut state = store.load();
    if available(&state, current).is_none() {
        state = check(store, url)?;
    }
    let Some(new) = available(&state, current).map(str::to_owned) else {
        return Ok((
            json!({"ok": true, "current": current, "updateAvailable": false}),
            0,
        ));
    };
    state.skipped = Some(new.clone());
    store.save(&state)?;
    Ok((json!({"ok": true, "skipped": new}), 0))
}

/// `update --auto on|off|status` and `update --notify on|off|status`.
fn configure(
    store: &Store,
    auto: Option<&str>,
    notify: Option<&str>,
) -> Result<(Value, u8), String> {
    let mut state = store.load();
    match (auto, notify) {
        (Some("on"), _) => {
            state.auto = true;
            store.save(&state)?;
        }
        (Some("off"), _) => {
            store.forget(&state)?;
            state = store.load();
        }
        (_, Some(value @ ("on" | "off"))) => {
            state.notify = value == "on";
            store.save(&state)?;
        }
        _ => {}
    }
    let file = store.state_path();
    let report = json!({
        "ok": true, "current": CURRENT, "notify": state.notify, "auto": state.auto,
        "lastCheckError": state.error, "lastAutoInstallError": state.auto_error,
        "stateFile": file.exists().then_some(file),
    });
    Ok((report, 0))
}

/// `update`: check now, confirm on a terminal, install, then offer auto once.
fn install_latest(
    store: &Store,
    url: &str,
    installer: Result<Installer, String>,
    current: &str,
    terminal: bool,
) -> Result<(Value, u8), String> {
    let state = check(store, url)?;
    let Some(new) = available(&state, current).map(str::to_owned) else {
        return Ok((
            json!({"ok": true, "current": current, "updateAvailable": false}),
            0,
        ));
    };
    let installer =
        installer.map_err(|e| format!("{e}; to update, run: {}", install_hint(&new)))?;
    if terminal {
        eprintln!("{TOOL} {current} → {new}");
        for line in state.notes.iter().flat_map(|notes| notes.lines()) {
            eprintln!("  {line}");
        }
        if !confirm(&format!("Install {new} now? [Y/n]"), true) {
            return Ok((json!({"ok": true, "installed": false, "latest": new}), 0));
        }
    }
    installer
        .install(&new, current, terminal)
        .map_err(|e| format!("{e}; {current} is unchanged"))?;
    let mut state = store.load();
    state.previous = Some(current.into());
    state.auto_error = None;
    if terminal && !state.auto && !state.auto_asked {
        state.auto = confirm("Install future updates automatically? [y/N]", false);
        state.auto_asked = true;
    }
    store.save(&state)?;
    let report = json!({
        "ok": true, "installed": new, "previous": current, "auto": state.auto,
        "note": "the new version runs from your next command",
    });
    Ok((report, 0))
}

/// `update --rollback`: back to the version that ran before the last update.
fn rollback(
    store: &Store,
    installer: Result<Installer, String>,
    current: &str,
    terminal: bool,
) -> Result<(Value, u8), String> {
    let kept = installer.as_ref().ok().and_then(Installer::kept_release);
    let previous = store.load().previous.or(kept).ok_or_else(|| {
        format!(
            "no earlier version is recorded or kept; to install one, run: {}",
            install_hint("X.Y.Z")
        )
    })?;
    installer?
        .rollback(&previous, current, terminal)
        .map_err(|e| format!("{e}; {current} is unchanged"))?;
    let mut state = store.load();
    state.previous = None;
    state.skipped = Some(current.into()); // do not offer the version just left
    store.save(&state)?;
    let report = json!({
        "ok": true, "installed": previous, "skipped": current,
        "note": "the restored version runs from your next command",
    });
    Ok((report, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        thread::{self, JoinHandle},
        time::Duration,
    };

    /// A private test folder, removed even when an assertion fails.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> TestDir {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                env::temp_dir().join(format!("umr-update-{name}-{}-{nanos}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            TestDir(fs::canonicalize(dir).unwrap())
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }

        fn store(&self) -> Store {
            Store {
                dir: self.path("cache"),
            }
        }

        /// A release folder `<home>/<version>` as the installer leaves it, with a
        /// stand-in binary that prints its version.
        fn release_folder(&self, root: &Path, version: &str, marker: bool) -> PathBuf {
            let rel = root.join(version);
            let bin = rel.join(BINARY);
            fs::create_dir_all(bin.parent().unwrap()).unwrap();
            fs::write(&bin, format!("#!/bin/sh\necho '{TOOL} {version}'\n")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(rel.join("VERSION"), version).unwrap();
            if marker {
                fs::write(rel.join(MARKER), "fixture\n").unwrap();
            }
            rel
        }

        /// An installer home with `current` at `version`.
        fn home(&self, version: &str) -> PathBuf {
            let home = self.path("home");
            fs::create_dir_all(&home).unwrap();
            let rel = self.release_folder(&home, version, true);
            symlink(&rel, home.join("current")).unwrap();
            home
        }

        /// A release dir (archive + SHA256SUMS) for `version`; `reports` is what
        /// its binary prints.
        fn release_dir(&self, version: &str, reports: &str) -> PathBuf {
            let stage = self.path(&format!("stage-{version}"));
            fs::create_dir_all(&stage).unwrap();
            let rel = self.release_folder(&stage, version, false);
            fs::write(
                rel.join(BINARY),
                format!("#!/bin/sh\necho '{TOOL} {reports}'\n"),
            )
            .unwrap();
            let out = self.path(&format!("release-{version}"));
            fs::create_dir_all(&out).unwrap();
            let tar = Command::new("tar")
                .arg("-czf")
                .arg(out.join(ASSET))
                .arg("-C")
                .arg(&rel)
                .args(["ultra-media-remote.app", "VERSION"])
                .output()
                .unwrap();
            assert!(tar.status.success());
            let sum = sha256_file(&out.join(ASSET)).unwrap();
            fs::write(out.join("SHA256SUMS"), format!("{sum}  {ASSET}\n")).unwrap();
            out
        }

        fn installer(&self, home: &Path, release: &Path) -> Installer {
            let release = release.to_string_lossy().into_owned();
            Installer {
                home: home.to_path_buf(),
                base: Box::new(move |_| release.clone()),
                team: None,
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn current_of(home: &Path) -> String {
        fs::read_link(home.join("current"))
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn release(tag: &str) -> String {
        json!({"tag_name": tag, "draft": false, "prerelease": false, "body": "## Fixes\n- Faster seek\n"})
            .to_string()
    }

    /// A one-shot local release API that hands back the raw request it saw.
    fn serve(body: String) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/repos/o/r/releases/latest",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, server)
    }

    fn closed_port_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/releases/latest", listener.local_addr().unwrap())
    }

    #[test]
    fn versions_compare_strictly_on_stable_releases() {
        assert!(is_newer("0.4.0", "0.3.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        for bad in [
            "",
            "v",
            "garbage",
            "1.0",
            "1.0.0.0",
            "1.0.0-rc.1",
            "1.0.0+b",
            "01.0.0",
            "1..0",
            "-1.0.0",
            " 1.0.0",
            "../1.0.0",
        ] {
            assert_eq!(parse_version(bad), None, "{bad:?} parsed");
            assert!(!is_newer(bad, "0.1.0"), "{bad:?} counted as newer");
        }
        assert!(!is_newer("9.9.9", "garbage"));
        assert!(install_target("0.3.0", "0.3.0").is_err());
        assert!(install_target("../x", "0.3.0").is_err());
        assert_eq!(install_target("v0.4.0", "0.3.0").unwrap(), "0.4.0");
    }

    #[test]
    fn release_record_ignores_drafts_prereleases_and_odd_tags() {
        for raw in [
            r#"{"tag_name":"v9.9.9","draft":true}"#,
            r#"{"tag_name":"v9.9.9","prerelease":true}"#,
            r#"{"tag_name":"v9.9.9-beta.1"}"#,
            r#"{"tag_name":"nightly","body":null}"#,
        ] {
            assert_eq!(parse_release(raw).unwrap(), None, "{raw}");
        }
        assert!(parse_release("<html>rate limited</html>").is_err());
        let (version, notes) = parse_release(&release("v9.9.9")).unwrap().unwrap();
        assert_eq!(version, "9.9.9");
        assert_eq!(notes, "Fixes\nFaster seek");
        let long = format!("{}\n\n- a\n- b\n- c\n- d\n- e\n- f", "x".repeat(300));
        let notes = short_notes(&long);
        assert_eq!(notes.lines().count(), 5);
        assert_eq!(notes.lines().next().unwrap().len(), 100);
    }

    #[test]
    fn automatic_checks_only_for_a_person_at_a_terminal() {
        let state = State::default();
        fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        }
        assert!(automatic(true, &state, env(&[])));
        assert!(!automatic(false, &state, env(&[])), "no terminal");
        assert!(!automatic(true, &state, env(&[("CI", "true")])));
        assert!(automatic(true, &state, env(&[("CI", "0")])));
        assert!(!automatic(
            true,
            &state,
            env(&[("ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK", "1")])
        ));
        for agent in ["CLAUDECODE", "CODEX_SANDBOX", "GEMINI_CLI"] {
            assert!(!automatic(true, &state, env(&[(agent, "1")])), "{agent}");
        }
        let opted_out = State {
            notify: false,
            ..State::default()
        };
        assert!(!automatic(true, &opted_out, env(&[])), "--notify off");
    }

    #[test]
    fn check_is_one_anonymous_get_and_is_cached_privately() {
        let dir = TestDir::new("check");
        let store = dir.store();
        let (url, server) = serve(release("v9.9.9"));
        let state = check(&store, &url).unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /repos/o/r/releases/latest "));
        assert!(request.contains(&format!("user-agent: {USER_AGENT}")));
        for leak in [CURRENT, "cookie", "authorization", "x-"] {
            assert!(!request.contains(leak), "request carries {leak:?}");
        }
        assert_eq!(state.latest.as_deref(), Some("9.9.9"));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&store.state_path()), 0o600);
        assert_eq!(mode(&store.dir), 0o700);
        assert_eq!(store.load(), state);
    }

    #[test]
    fn a_failed_check_records_its_cause_and_is_not_due_again_today() {
        let dir = TestDir::new("fail");
        let store = dir.store();
        assert!(check(&store, &closed_port_url()).is_err());
        let state = store.load();
        assert!(state
            .error
            .as_deref()
            .unwrap()
            .contains("release check failed"));
        assert!(!due(state.checked_at, now()));
        assert!(due(state.checked_at, now() + DAY));
        assert!(due(Some(now() + 3600), now()), "clock set backwards");
    }

    #[test]
    fn notice_is_daily_per_version_and_respects_skip() {
        let mut state = State {
            latest: Some("0.9.0".into()),
            ..State::default()
        };
        let lines = notice(&state, "0.3.0", 1000).unwrap();
        assert_eq!(
            lines[0],
            "ultra-media-remote 0.9.0 available (you have 0.3.0) · run: ultra-media-remote update"
        );
        assert!(lines[1].contains("update --auto on"));
        state.shown = Some("0.9.0".into());
        state.shown_at = Some(1000);
        assert!(notice(&state, "0.3.0", 1000 + 60).is_none(), "same day");
        assert!(notice(&state, "0.3.0", 1000 + DAY).is_some(), "next day");
        state.latest = Some("0.10.0".into());
        assert!(
            notice(&state, "0.3.0", 1000 + 60).is_some(),
            "newer version"
        );
        state.skipped = Some("0.10.0".into());
        assert!(notice(&state, "0.3.0", 1000 + DAY).is_none(), "skipped");
        state.latest = Some("0.3.0".into());
        assert!(notice(&state, "0.3.0", 0).is_none(), "not newer");
        let mut out = Vec::new();
        write_notice(&mut out, &["a".into(), "b".into()], false).unwrap();
        assert_eq!(out, b"a\nb\n");
    }

    #[test]
    fn auto_off_deletes_the_cache_and_notify_off_is_remembered() {
        let dir = TestDir::new("forget");
        let store = dir.store();
        configure(&store, Some("on"), None).unwrap();
        assert!(store.load().auto);
        store.log("x");
        configure(&store, Some("off"), None).unwrap();
        assert!(!store.dir.exists(), "auto off leaves no files");
        configure(&store, None, Some("off")).unwrap();
        configure(&store, Some("off"), None).unwrap();
        assert!(!store.load().notify, "notify off survives auto off");
        assert!(!store.log_path().exists());
    }

    #[test]
    fn log_stays_under_its_limit() {
        let mut bytes = Vec::new();
        for i in 0..20_000 {
            bytes.extend_from_slice(format!("{i} a line of update log text\n").as_bytes());
            trim_log(&mut bytes);
        }
        assert!(bytes.len() <= LOG_LIMIT);
        assert!(bytes.starts_with(b"1") || bytes[0].is_ascii_digit());
        assert!(bytes.ends_with(b"19999 a line of update log text\n"));
    }

    #[test]
    fn install_verifies_switches_and_keeps_one_rollback() {
        let dir = TestDir::new("install");
        let home = dir.home("0.3.0");
        dir.release_folder(&home, "0.1.0", true); // older release: pruned
        dir.release_folder(&home, "0.2.0", false); // not ours: left alone
        let release = dir.release_dir("0.4.0", "0.4.0");
        let installer = dir.installer(&home, &release);
        installer.install("0.4.0", "0.3.0", false).unwrap();
        assert_eq!(current_of(&home), "0.4.0");
        assert!(home.join("0.4.0").join(MARKER).is_file());
        assert!(home.join("0.3.0").is_dir(), "rollback copy kept");
        assert!(!home.join("0.1.0").exists(), "older release pruned");
        assert!(home.join("0.2.0").exists(), "foreign folder untouched");
        let leftovers: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "work folder removed");

        // The new release ran once: the rollback copy goes.
        settle_at(&home.join("current").join(BINARY), true, now());
        assert!(!home.join("0.3.0").exists());

        // Rollback reinstalls when the kept folder is gone.
        let back = dir.release_dir("0.3.0", "0.3.0");
        dir.installer(&home, &back)
            .rollback("0.3.0", "0.4.0", false)
            .unwrap();
        assert_eq!(current_of(&home), "0.3.0");
        assert!(home.join("0.4.0").is_dir(), "left release kept as rollback");
        dir.installer(&home, &release)
            .rollback("0.4.0", "0.3.0", false)
            .unwrap();
        assert_eq!(current_of(&home), "0.4.0", "kept folder switched back");
    }

    #[test]
    fn install_refuses_bad_checksum_wrong_version_and_foreign_folders() {
        let dir = TestDir::new("refuse");
        let home = dir.home("0.3.0");
        let release = dir.release_dir("0.4.0", "0.4.0");
        let sums = release.join("SHA256SUMS");
        let good = fs::read_to_string(&sums).unwrap();
        fs::write(&sums, format!("{}  {ASSET}\n", "0".repeat(64))).unwrap();
        let err = dir
            .installer(&home, &release)
            .install("0.4.0", "0.3.0", false)
            .unwrap_err();
        assert!(err.contains("checksum mismatch"), "{err}");
        fs::write(&sums, good).unwrap();

        let liar = dir.release_dir("0.5.0", "0.4.9");
        let err = dir
            .installer(&home, &liar)
            .install("0.5.0", "0.3.0", false)
            .unwrap_err();
        assert!(err.contains("does not report 0.5.0"), "{err}");

        dir.release_folder(&home, "0.4.0", false);
        let err = dir
            .installer(&home, &release)
            .install("0.4.0", "0.3.0", false)
            .unwrap_err();
        assert!(err.contains("refusing to replace"), "{err}");
        assert_eq!(current_of(&home), "0.3.0", "current never moved");
        assert!(fs::read_dir(&home)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(WORK_PREFIX)));
    }

    #[test]
    fn only_an_installer_copy_that_is_current_updates_itself() {
        let dir = TestDir::new("home");
        let home = dir.home("0.3.0");
        let exe = home.join("0.3.0").join(BINARY);
        assert_eq!(installer_home(&exe).as_deref(), Some(home.as_path()));
        assert_eq!(
            installer_home(&home.join("current").join(BINARY)).as_deref(),
            Some(home.as_path())
        );
        let stale = dir.release_folder(&home, "0.2.0", true).join(BINARY);
        assert_eq!(installer_home(&stale), None, "not the current release");
        let loose = dir.release_folder(&dir.path("loose"), "0.3.0", false);
        assert_eq!(installer_home(&loose.join(BINARY)), None, "no marker");
    }

    #[test]
    fn a_second_update_of_the_same_copy_is_refused_while_one_runs() {
        let dir = TestDir::new("lock");
        let home = dir.home("0.3.0");
        let release = dir.release_dir("0.4.0", "0.4.0");
        let held = Lock::take(&home).unwrap();
        let err = dir
            .installer(&home, &release)
            .install("0.4.0", "0.3.0", false)
            .unwrap_err();
        assert!(err.contains("another update"), "{err}");
        drop(held);
        dir.installer(&home, &release)
            .install("0.4.0", "0.3.0", false)
            .unwrap();
        assert!(!home.join(".update-lock").exists(), "lock released");
        assert_eq!(current_of(&home), "0.4.0");
    }

    #[test]
    fn rollback_finds_the_kept_release_without_state() {
        let dir = TestDir::new("kept");
        let home = dir.home("0.4.0");
        dir.release_folder(&home, "0.3.0", true);
        let installer = dir.installer(&home, &dir.path("none"));
        assert_eq!(installer.kept_release().as_deref(), Some("0.3.0"));
        dir.release_folder(&home, "0.2.0", true);
        assert_eq!(installer.kept_release(), None, "ambiguous");
        assert!(!release_name("v0.1.0"));
        assert!(release_name("0.1.0"));
    }

    #[test]
    fn unsigned_apps_fail_the_signature_check() {
        let dir = TestDir::new("sign");
        let rel = dir.release_folder(&dir.path("r"), "0.4.0", false);
        let app = rel.join("ultra-media-remote.app");
        let err = verify_signature(&app, "ABCDE12345").unwrap_err();
        assert!(err.contains("not signed by team ABCDE12345"), "{err}");
        assert_eq!(team_id(&app), None);
    }

    #[test]
    fn hints_name_the_right_command_for_each_install_kind() {
        let dir = TestDir::new("hint");
        let root = dir.path("cargo-root");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join(".crates2.json"), "{}").unwrap();
        fs::write(root.join("bin").join(TOOL), "").unwrap();
        let cargo = hint_for(Some(&root.join("bin").join(TOOL)), "0.4.0");
        assert!(cargo.starts_with("cargo install --git "), "{cargo}");
        assert!(cargo.contains("--tag v0.4.0"), "{cargo}");
        let other = hint_for(Some(&dir.path("elsewhere/ultra-media-remote")), "0.4.0");
        assert!(
            other.contains("ULTRA_MEDIA_REMOTE_VERSION=0.4.0 sh"),
            "{other}"
        );
    }

    #[test]
    fn update_arguments_parse_strictly() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_action(&args(&[])).unwrap(), Action::Install);
        assert_eq!(parse_action(&args(&["--check"])).unwrap(), Action::Check);
        assert_eq!(
            parse_action(&args(&["--auto", "on"])).unwrap(),
            Action::Auto("on".into())
        );
        assert_eq!(
            parse_action(&args(&["--notify", "status"])).unwrap(),
            Action::Notify("status".into())
        );
        for bad in [
            &["--auto"][..],
            &["--auto", "maybe"],
            &["--check", "--skip"],
            &["now"],
        ] {
            assert!(parse_action(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
