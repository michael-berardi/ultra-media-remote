//! `ultra-media-remote` — macOS Now Playing info and media transport control.
//!
//! The crate wraps Apple's private MediaRemote framework through a small Swift
//! static library (`native/UltraMediaRemote`) that is compiled by the build
//! script and linked into the Rust binary. All MediaRemote access happens at
//! runtime via dlopen/dlsym: nothing is statically linked, and when the
//! framework or its symbols are absent every API degrades gracefully to
//! "unavailable" instead of failing.
//!
//! On non-macOS targets the FFI layer compiles to stubs so the crate still
//! builds; all queries simply report unavailable.

use std::ffi::{c_char, CStr};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

/// Current Now Playing metadata as exposed by the system media session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NowPlaying {
    /// Title of the current track, when reported.
    pub title: Option<String>,
    /// Artist of the current track, when reported.
    pub artist: Option<String>,
    /// Album of the current track, when reported.
    pub album: Option<String>,
    /// Localized name of the owning application, when resolvable.
    pub app_name: Option<String>,
    /// Bundle identifier of the owning application, when resolvable.
    pub bundle_id: Option<String>,
    /// MediaRemote's stable identifier for the active item, when reported.
    pub unique_identifier: Option<String>,
    /// Content catalog identifier for the active item, when reported.
    pub content_item_identifier: Option<String>,
    /// MediaPlayer media type (`1` audio, `2` video), when reported.
    pub media_type: Option<i64>,
    /// Process identifier of the owning application. Best-effort: some
    /// browser sessions expose metadata but withhold the PID.
    pub pid: Option<i32>,
    /// Elapsed playback position in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<f64>,
    /// Total track duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,
    /// Playback state. `None` when the session exposes no rate or playing flag.
    pub is_playing: Option<bool>,
    /// Cover art as a bounded `data:<mime>;base64,<data>` URL. Populated only
    /// by the adapter path; the direct MediaRemote dlopen path never yields it.
    pub artwork_data_url: Option<String>,
    /// Current positive rating, when exposed by the active media session.
    pub is_liked: Option<bool>,
    /// Current negative rating, when exposed by the active media session.
    pub is_disliked: Option<bool>,
    /// Whether the active media session accepts a positive rating command.
    pub supports_like: bool,
    /// Whether the active media session accepts a negative rating command.
    pub supports_dislike: bool,
}

/// Media transport capabilities of the current system media session.
///
/// Secondary commands are `true` only when MediaRemote's command discovery
/// proves they are both supported and enabled for the active player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub play_pause: bool,
    pub previous: bool,
    pub next: bool,
    pub like: bool,
    pub dislike: bool,
}

/// Combined media state for one point-in-time read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaSnapshot {
    pub now_playing: Option<NowPlaying>,
    pub capabilities: Capabilities,
    pub output_volume: Option<f64>,
}

/// Transport commands accepted by [`transport_send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportCommand {
    PlayPause,
    Next,
    Previous,
    Like,
    Dislike,
}

impl TransportCommand {
    /// Zero-based `MRMediaRemoteCommand` value sent to MediaRemote.
    pub const fn code(self) -> u32 {
        match self {
            TransportCommand::PlayPause => 2,
            TransportCommand::Next => 4,
            TransportCommand::Previous => 5,
            TransportCommand::Like => 106,
            TransportCommand::Dislike => 107,
        }
    }
}

/// Unknown MediaRemote command code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownCommand(pub u32);

impl TryFrom<u32> for TransportCommand {
    type Error = UnknownCommand;

    fn try_from(code: u32) -> Result<Self, Self::Error> {
        match code {
            2 => Ok(TransportCommand::PlayPause),
            4 => Ok(TransportCommand::Next),
            5 => Ok(TransportCommand::Previous),
            106 => Ok(TransportCommand::Like),
            107 => Ok(TransportCommand::Dislike),
            other => Err(UnknownCommand(other)),
        }
    }
}

// MARK: - Pure mapping logic (unit tested)

/// Parses a now-playing payload (the JSON produced by the Swift layer) into a
/// [`NowPlaying`]. Missing fields map to `None`; invalid values (negative or
/// non-finite times, non-positive PIDs) are dropped rather than propagated.
pub(crate) fn parse_now_playing(value: &serde_json::Value) -> Option<NowPlaying> {
    let object = value.as_object()?;

    fn string(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
        object.get(key).and_then(|v| v.as_str()).map(str::to_owned)
    }

    fn time(object: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
        object
            .get(key)
            .and_then(|v| v.as_f64())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
    }

    Some(NowPlaying {
        title: string(object, "title"),
        artist: string(object, "artist"),
        album: string(object, "album"),
        app_name: string(object, "app_name"),
        bundle_id: string(object, "bundle_id"),
        unique_identifier: string(object, "unique_identifier"),
        content_item_identifier: string(object, "content_item_identifier"),
        media_type: object.get("media_type").and_then(|v| v.as_i64()),
        pid: object
            .get("pid")
            .and_then(|v| v.as_i64())
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as i64)
            .map(|pid| pid as i32),
        elapsed_seconds: time(object, "elapsed_seconds"),
        duration_seconds: time(object, "duration_seconds"),
        is_playing: object.get("is_playing").and_then(|v| v.as_bool()),
        // The direct MediaRemote path exposes no artwork.
        artwork_data_url: None,
        is_liked: object.get("is_liked").and_then(|v| v.as_bool()),
        is_disliked: object.get("is_disliked").and_then(|v| v.as_bool()),
        supports_like: object
            .get("supports_like")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        supports_dislike: object
            .get("supports_dislike")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// Returns a confirmed YouTube video ID from MediaRemote identifier fields.
///
/// Identifiers are accepted only when they are exactly the eleven-character
/// YouTube alphabet, or when they are an explicit YouTube URL. Titles, UUIDs,
/// and arbitrary substrings are never treated as video IDs.
pub fn youtube_video_id(
    unique_identifier: Option<&str>,
    content_item_identifier: Option<&str>,
) -> Option<String> {
    let mut resolved = None;
    for value in [unique_identifier, content_item_identifier]
        .into_iter()
        .flatten()
        .filter_map(youtube_video_id_from_value)
    {
        match resolved.as_deref() {
            None => resolved = Some(value),
            Some(existing) if existing == value => {}
            Some(_) => return None,
        }
    }
    resolved
}

/// Extracts a YouTube video ID from an explicit YouTube URL.
pub fn youtube_video_id_from_url(value: &str) -> Option<String> {
    let value = value.trim();
    let scheme_end = value.find("://")?;
    let scheme = &value[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority_start = scheme_end + 3;
    let authority_end = value[authority_start..]
        .find(['/', '?', '#'])
        .map(|offset| authority_start + offset)
        .unwrap_or(value.len());
    let host = value[authority_start..authority_end]
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(&value[authority_start..authority_end])
        .split(':')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let remainder = &value[authority_end..];
    if host == "youtu.be" {
        return youtube_id(remainder.trim_start_matches('/').split(['?', '#']).next()?);
    }
    if !matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com"
    ) {
        return None;
    }
    let (path, query) = remainder
        .split_once('?')
        .map_or((remainder, ""), |(path, query)| (path, query));
    if path.eq_ignore_ascii_case("/watch") {
        let candidate = query.split('&').find_map(|part| part.strip_prefix("v="))?;
        return youtube_id(candidate.split('#').next().unwrap_or(candidate));
    }
    let path_id = path
        .strip_prefix("/shorts/")
        .or_else(|| path.strip_prefix("/embed/"))
        .or_else(|| path.strip_prefix("/live/"))?;
    youtube_id(path_id.split(['/', '?', '#']).next().unwrap_or(path_id))
}

fn youtube_video_id_from_value(value: &str) -> Option<String> {
    youtube_id(value).or_else(|| youtube_video_id_from_url(value))
}

fn youtube_id(value: &str) -> Option<String> {
    (value.len() == 11
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
    .then(|| value.to_owned())
}

/// Resolves transport capabilities. Direct MediaRemote discovery is
/// authoritative when available; the staged adapter is the entitled fallback
/// on modern macOS and accepts the standard play/pause, previous, and next
/// command contract used by Music, Spotify, and browser media sessions.
pub(crate) fn resolve_capabilities(
    send_available: bool,
    enabled_codes: &[u32],
    adapter_available: bool,
) -> Capabilities {
    Capabilities {
        play_pause: send_available,
        previous: send_available
            && (adapter_available || enabled_codes.contains(&TransportCommand::Previous.code())),
        next: send_available
            && (adapter_available || enabled_codes.contains(&TransportCommand::Next.code())),
        like: send_available && enabled_codes.contains(&TransportCommand::Like.code()),
        dislike: send_available && enabled_codes.contains(&TransportCommand::Dislike.code()),
    }
}

// MARK: - FFI

/// Buffer size for enabled-command discovery; MediaRemote's command set is far
/// smaller than this.
const COMMAND_CODE_CAPACITY: u32 = 32;

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{c_char, c_int};

    type SubscribeCallback = unsafe extern "C" fn(u64, *const c_char);

    unsafe extern "C" {
        pub safe fn umr_now_playing_available() -> c_int;
        pub safe fn umr_transport_available() -> c_int;
        pub fn umr_output_volume(volume_out: *mut f64) -> c_int;
        pub safe fn umr_set_output_volume(volume: f64) -> c_int;
        pub fn umr_now_playing_fetch(timeout_ms: u32) -> *mut c_char;
        pub fn umr_free_string(s: *mut c_char);
        pub fn umr_transport_supported_commands(codes: *mut u32, capacity: u32) -> c_int;
        pub fn umr_transport_send(code: u32) -> c_int;
        pub safe fn umr_seek(seconds: f64) -> c_int;
        pub fn umr_now_playing_subscribe(
            callback: Option<SubscribeCallback>,
            context: u64,
            interval_ms: u32,
        ) -> u64;
        pub fn umr_now_playing_unsubscribe(handle: u64);

        #[cfg(feature = "spectrum")]
        pub safe fn umr_spectrum_permission_granted() -> c_int;
        #[cfg(feature = "spectrum")]
        pub safe fn umr_spectrum_request_permission() -> c_int;
        #[cfg(feature = "spectrum")]
        pub fn umr_spectrum_start() -> u64;
        #[cfg(feature = "spectrum")]
        pub fn umr_spectrum_start_with_existing_consent() -> u64;
        #[cfg(feature = "spectrum")]
        pub fn umr_spectrum_fetch(handle: u64) -> *mut c_char;
        #[cfg(feature = "spectrum")]
        pub fn umr_spectrum_stop(handle: u64);
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    use std::ffi::{c_char, c_int};

    pub fn umr_now_playing_available() -> c_int {
        0
    }
    pub fn umr_transport_available() -> c_int {
        0
    }
    pub unsafe fn umr_output_volume(_volume_out: *mut f64) -> c_int {
        0
    }
    pub fn umr_set_output_volume(_volume: f64) -> c_int {
        0
    }
    pub unsafe fn umr_now_playing_fetch(_timeout_ms: u32) -> *mut c_char {
        std::ptr::null_mut()
    }
    pub unsafe fn umr_free_string(_s: *mut c_char) {}
    pub unsafe fn umr_transport_supported_commands(_codes: *mut u32, _capacity: u32) -> c_int {
        0
    }
    pub unsafe fn umr_transport_send(_code: u32) -> c_int {
        0
    }
    pub fn umr_seek(_seconds: f64) -> c_int {
        0
    }
    pub unsafe fn umr_now_playing_subscribe(
        _callback: Option<unsafe extern "C" fn(u64, *const c_char)>,
        _context: u64,
        _interval_ms: u32,
    ) -> u64 {
        0
    }
    #[cfg(feature = "spectrum")]
    pub fn umr_spectrum_permission_granted() -> c_int {
        0
    }
    #[cfg(feature = "spectrum")]
    pub fn umr_spectrum_request_permission() -> c_int {
        0
    }
    #[cfg(feature = "spectrum")]
    pub unsafe fn umr_spectrum_start() -> u64 {
        0
    }
    #[cfg(feature = "spectrum")]
    pub unsafe fn umr_spectrum_start_with_existing_consent() -> u64 {
        0
    }
    #[cfg(feature = "spectrum")]
    pub unsafe fn umr_spectrum_fetch(_handle: u64) -> *mut c_char {
        std::ptr::null_mut()
    }
    #[cfg(feature = "spectrum")]
    pub unsafe fn umr_spectrum_stop(_handle: u64) {}
    pub unsafe fn umr_now_playing_unsubscribe(_handle: u64) {}
}

fn timeout_ms(timeout: Duration) -> u32 {
    timeout.as_millis().try_into().unwrap_or(u32::MAX)
}

/// Parses a JSON C string owned by the Swift layer into a [`NowPlaying`].
fn parse_json_c_string(json: *const c_char) -> Option<NowPlaying> {
    if json.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(json) }.to_str().ok()?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    parse_now_playing(&value)
}

// MARK: - MediaRemoteAdapter (macOS 15.4+ metadata path)
// Apple restricts direct `MRMediaRemoteGetNowPlayingInfo` responses to
// eligible processes on macOS 15.4+. The bundled BSD-3-Clause MediaRemoteAdapter
// (third_party/mediaremote-adapter, Jonas van den Berg and contributors) works
// around this by loading its own MediaRemoteAdapter.framework through
// /usr/bin/perl, which carries the necessary system entitlement. It prints the
// current Now Playing dictionary as JSON on stdout.

/// Upper bound for adapter stdout; larger output is treated as corrupt.
const MAX_ADAPTER_OUTPUT_BYTES: usize = 1536 * 1024;

struct AdapterCache {
    fetched_at: Option<Instant>,
    value: Option<NowPlaying>,
}

static ADAPTER_CACHE: LazyLock<Mutex<AdapterCache>> = LazyLock::new(|| {
    Mutex::new(AdapterCache {
        fetched_at: None,
        value: None,
    })
});

/// Locates the staged adapter directory. Candidates, in order: the
/// `ULTRA_MEDIA_REMOTE_ADAPTER_DIR` environment variable, a `mediaremote`
/// directory next to the executable, and a `Resources/mediaremote` directory
/// of an app bundle containing the executable. A candidate counts only when
/// it contains both adapter parts.
fn adapter_directory() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("ULTRA_MEDIA_REMOTE_ADAPTER_DIR") {
        candidates.push(PathBuf::from(path));
    }
    if let Ok(executable) = std::env::current_exe() {
        if let Some(binary_dir) = executable.parent() {
            candidates.push(binary_dir.join("mediaremote"));
            if let Some(contents_dir) = binary_dir.parent() {
                candidates.push(contents_dir.join("Resources").join("mediaremote"));
            }
        }
    }
    candidates.into_iter().find(|directory| {
        directory.join("mediaremote-adapter.pl").is_file()
            && directory.join("MediaRemoteAdapter.framework").is_dir()
    })
}

fn adapter_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn adapter_identifier(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}
fn adapter_seconds(value: &serde_json::Value, key: &str) -> Option<f64> {
    value
        .get(key)
        .and_then(serde_json::Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Upper bound for base64 artwork payloads; larger covers are dropped.
const MAX_ARTWORK_BASE64_BYTES: usize = 1024 * 1024;

/// Parses the adapter's JSON payload into a [`NowPlaying`]. Payloads without
/// any metadata are rejected so empty sessions do not shadow the fallback.
pub(crate) fn parse_adapter_now_playing(value: &serde_json::Value) -> Option<NowPlaying> {
    let title = adapter_string(value, "title");
    let artist = adapter_string(value, "artist");
    let album = adapter_string(value, "album");
    let unique_identifier = adapter_identifier(value, "uniqueIdentifier");
    let content_item_identifier = adapter_identifier(value, "contentItemIdentifier");
    let media_type = value.get("mediaType").and_then(serde_json::Value::as_i64);
    if title.is_none()
        && artist.is_none()
        && album.is_none()
        && unique_identifier.is_none()
        && content_item_identifier.is_none()
        && media_type.is_none()
    {
        return None;
    }
    Some(NowPlaying {
        title,
        artist,
        album,
        app_name: adapter_string(value, "applicationName"),
        bundle_id: adapter_string(value, "bundleIdentifier"),
        unique_identifier,
        content_item_identifier,
        media_type,
        pid: value
            .get("processIdentifier")
            .and_then(serde_json::Value::as_i64)
            .filter(|pid| *pid > 0)
            .and_then(|pid| i32::try_from(pid).ok()),
        elapsed_seconds: adapter_seconds(value, "elapsedTimeNow")
            .or_else(|| adapter_seconds(value, "elapsedTime")),
        duration_seconds: adapter_seconds(value, "duration"),
        is_playing: value
            .get("playing")
            .and_then(serde_json::Value::as_bool)
            .or_else(|| {
                value
                    .get("playbackRate")
                    .and_then(serde_json::Value::as_f64)
                    .map(|rate| rate > 0.0)
            }),
        artwork_data_url: adapter_string(value, "artworkData").and_then(|data| {
            let mime = adapter_string(value, "artworkMimeType")?;
            (mime.starts_with("image/") && data.len() <= MAX_ARTWORK_BASE64_BYTES)
                .then(|| format!("data:{mime};base64,{data}"))
        }),
        is_liked: value.get("isLiked").and_then(serde_json::Value::as_bool),
        is_disliked: value.get("isBanned").and_then(serde_json::Value::as_bool),
        supports_like: value
            .get("supportsIsLiked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        supports_dislike: value
            .get("supportsIsBanned")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Spawns `/usr/bin/perl mediaremote-adapter.pl MediaRemoteAdapter.framework
/// get --now` and parses its JSON stdout. Output beyond
/// [`MAX_ADAPTER_OUTPUT_BYTES`] or a nonzero exit is rejected; stderr is
/// discarded.
fn read_adapter_now_playing() -> Option<NowPlaying> {
    let directory = adapter_directory()?;
    let mut child = Command::new("/usr/bin/perl")
        .arg(directory.join("mediaremote-adapter.pl"))
        .arg(directory.join("MediaRemoteAdapter.framework"))
        .arg("get")
        .arg("--now")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut bytes = Vec::with_capacity(64 * 1024);
    let mut stdout = child.stdout.take()?;
    if stdout
        .by_ref()
        .take((MAX_ADAPTER_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    drop(stdout);
    if bytes.len() > MAX_ADAPTER_OUTPUT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    if !child.wait().ok()?.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    parse_adapter_now_playing(&value)
}

/// Reads Control Center metadata through the bundled MediaRemote adapter,
/// cached for 2 seconds to keep repeated polls cheap.
fn adapter_now_playing() -> Option<NowPlaying> {
    let mut cache = ADAPTER_CACHE.lock().ok()?;
    if cache
        .fetched_at
        .is_some_and(|time| time.elapsed() < Duration::from_secs(2))
    {
        return cache.value.clone();
    }
    let value = read_adapter_now_playing();
    cache.fetched_at = Some(Instant::now());
    cache.value = value.clone();
    value
}

fn invalidate_adapter_cache() {
    if let Ok(mut cache) = ADAPTER_CACHE.lock() {
        cache.fetched_at = None;
    }
}

// MARK: - Public API

/// Returns whether runtime MediaRemote metadata access is available on this
/// system. When false, [`now_playing_fetch`] always returns `None`.
pub fn now_playing_available() -> bool {
    sys::umr_now_playing_available() != 0
}

/// Returns whether runtime MediaRemote transport delivery is available. When
/// false, [`transport_send`] always returns `false`.
pub fn transport_available() -> bool {
    sys::umr_transport_available() != 0
}

fn valid_output_volume(volume: f64) -> bool {
    volume.is_finite() && (0.0..=1.0).contains(&volume)
}

/// Reads the default system output volume as a normalized scalar in [0, 1].
/// Returns `None` when the platform or output device does not expose a
/// readable scalar volume control.
pub fn output_volume() -> Option<f64> {
    let mut volume = 0.0;
    let available = unsafe { sys::umr_output_volume(&mut volume) } != 0;
    available
        .then_some(volume)
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
}

/// Sets the default system output volume as a normalized scalar in [0, 1].
/// Invalid values and unavailable or read-only devices return `false`.
pub fn set_output_volume(volume: f64) -> bool {
    valid_output_volume(volume) && sys::umr_set_output_volume(volume) != 0
}

/// Fetches the current Now Playing snapshot. Returns `None` when MediaRemote
/// is unavailable, nothing is playing, or the reply did not arrive within
/// `timeout`.
pub fn now_playing_fetch(timeout: Duration) -> Option<NowPlaying> {
    // The perl adapter runs under /usr/bin/perl's system entitlement and is
    // the only path that yields metadata on macOS 15.4+; results are cached
    // for 2 seconds. Direct dlopen'd MediaRemote access remains as the
    // fallback for older systems and entitlement-less environments where the
    // adapter is not staged.
    if let Some(snapshot) = adapter_now_playing() {
        return Some(snapshot);
    }
    let pointer = unsafe { sys::umr_now_playing_fetch(timeout_ms(timeout)) };
    if pointer.is_null() {
        return None;
    }
    let parsed = parse_json_c_string(pointer);
    unsafe { sys::umr_free_string(pointer) };
    parsed
}

/// Reads transport capabilities for the current system media session.
/// The staged adapter restores the standard three-command contract on modern
/// macOS where direct command discovery is entitlement-restricted.
pub fn transport_capabilities() -> Capabilities {
    let adapter_available = adapter_directory().is_some();
    let send_available = transport_available() || adapter_available;
    let mut codes = [0u32; COMMAND_CODE_CAPACITY as usize];
    let count =
        unsafe { sys::umr_transport_supported_commands(codes.as_mut_ptr(), codes.len() as u32) };
    let count = count.clamp(0, codes.len() as i32) as usize;
    resolve_capabilities(send_available, &codes[..count], adapter_available)
}

fn compose_media_snapshot(
    now_playing: Option<NowPlaying>,
    capabilities: Capabilities,
    output_volume: Option<f64>,
) -> MediaSnapshot {
    MediaSnapshot {
        now_playing,
        capabilities,
        output_volume,
    }
}

/// Reads metadata, transport capabilities, and default-output volume for one
/// point-in-time media snapshot. Individual unavailable values remain `None`
/// or all-false rather than invalidating the rest of the snapshot.
pub fn media_snapshot(timeout: Duration) -> MediaSnapshot {
    compose_media_snapshot(
        now_playing_fetch(timeout),
        transport_capabilities(),
        output_volume(),
    )
}

/// Sends a transport command through runtime MediaRemote access. Returns
/// `false` when no send path is available or delivery failed.
pub fn transport_send(command: TransportCommand) -> bool {
    // The adapter delivers under /usr/bin/perl's system entitlement and is
    // the reliable path on macOS 15.4+; the direct dlopen'd send API remains
    // as fallback when the adapter is absent or rejects delivery.
    let sent = resolve_transport_send(adapter_send(command.code()), || unsafe {
        sys::umr_transport_send(command.code()) != 0
    });
    if sent {
        invalidate_adapter_cache();
    }
    sent
}

fn resolve_transport_send(
    adapter_result: Option<bool>,
    direct_send: impl FnOnce() -> bool,
) -> bool {
    match adapter_result {
        Some(true) => true,
        Some(false) | None => direct_send(),
    }
}

/// Seeks the active now-playing timeline to an absolute position in seconds.
/// The staged adapter is preferred; direct runtime MediaRemote access is the
/// fallback. Invalid or negative positions are rejected without delivery.
pub fn seek(position_seconds: f64) -> bool {
    let Some(position_micros) = seek_position_micros(position_seconds) else {
        return false;
    };
    let sent = resolve_transport_send(adapter_seek(position_micros), || {
        sys::umr_seek(position_seconds) != 0
    });
    if sent {
        invalidate_adapter_cache();
    }
    sent
}

fn seek_position_micros(position_seconds: f64) -> Option<i64> {
    if !position_seconds.is_finite() || position_seconds < 0.0 {
        return None;
    }
    let position_micros = position_seconds * 1_000_000.0;
    (position_micros <= i64::MAX as f64).then(|| position_micros.round() as i64)
}

/// Sends one transport command through the staged adapter, mirroring the
/// metadata path's process model. Returns `None` when no adapter is staged or
/// it could not be spawned; `Some(false)` covers both delivery failure and
/// the adapter rejecting the command code.
fn adapter_send(code: u32) -> Option<bool> {
    let directory = adapter_directory()?;
    let status = Command::new("/usr/bin/perl")
        .arg(directory.join("mediaremote-adapter.pl"))
        .arg(directory.join("MediaRemoteAdapter.framework"))
        .arg("send")
        .arg(code.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

fn adapter_seek(position_micros: i64) -> Option<bool> {
    let directory = adapter_directory()?;
    let status = Command::new("/usr/bin/perl")
        .arg(directory.join("mediaremote-adapter.pl"))
        .arg(directory.join("MediaRemoteAdapter.framework"))
        .arg("seek")
        .arg(position_micros.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

// MARK: - Spectrum (opt-in via the `spectrum` cargo feature)

/// Visual band frequencies in Hz, ordered low to high. Levels returned by
/// [`spectrum_start`] map one-to-one onto these bins.
#[cfg(feature = "spectrum")]
pub const SPECTRUM_BAND_FREQUENCIES_HZ: [f64; 11] = [
    63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 3_500.0, 5_000.0, 7_500.0, 10_000.0, 14_000.0,
];

/// Parses a spectrum JSON array (as produced by the Swift layer) into
/// normalized band levels. Requires exactly [`SPECTRUM_BAND_FREQUENCIES_HZ`]
/// values; each is clamped into [0, 1].
#[cfg(feature = "spectrum")]
pub(crate) fn parse_spectrum(text: &str) -> Option<Vec<f32>> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let array = value.as_array()?;
    if array.len() != SPECTRUM_BAND_FREQUENCIES_HZ.len() {
        return None;
    }
    Some(
        array
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32)
            .collect(),
    )
}

/// Live system-output spectrum handle. Dropping the value stops the capture
/// and releases its resources.
#[cfg(feature = "spectrum")]
pub struct Spectrum {
    handle: u64,
}

#[cfg(feature = "spectrum")]
impl Spectrum {
    /// Returns the latest 11-band spectrum as normalized levels in [0, 1],
    /// ordered low to high frequency. `None` when the session died.
    pub fn fetch(&self) -> Option<Vec<f32>> {
        #[cfg(target_os = "macos")]
        {
            let pointer = unsafe { sys::umr_spectrum_fetch(self.handle) };
            if pointer.is_null() {
                return None;
            }
            let parsed = unsafe { CStr::from_ptr(pointer) }
                .to_str()
                .ok()
                .and_then(parse_spectrum);
            unsafe { sys::umr_free_string(pointer) };
            parsed
        }
        #[cfg(not(target_os = "macos"))]
        None
    }
}

#[cfg(feature = "spectrum")]
impl Drop for Spectrum {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        unsafe {
            sys::umr_spectrum_stop(self.handle)
        };
    }
}

/// Returns whether Screen Recording permission is already granted for the
/// calling process. This preflight never prompts.
#[cfg(feature = "spectrum")]
pub fn spectrum_permission_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        return sys::umr_spectrum_permission_granted() != 0;
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Requests Screen Recording permission for the calling process. This may
/// display the macOS consent prompt and must only run from an explicit user
/// action. Use [`spectrum_permission_granted`] for non-prompting checks.
#[cfg(feature = "spectrum")]
pub fn spectrum_request_permission() -> bool {
    #[cfg(target_os = "macos")]
    {
        return sys::umr_spectrum_request_permission() != 0;
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Starts capturing system-output audio for spectral analysis. Opt-in,
/// macOS 15+ at runtime, and requires Screen Recording permission already
/// granted for the calling app. This function never triggers a prompt.
#[cfg(feature = "spectrum")]
pub fn spectrum_start() -> Option<Spectrum> {
    #[cfg(target_os = "macos")]
    {
        if !spectrum_permission_granted() {
            return None;
        }
        let handle = unsafe { sys::umr_spectrum_start() };
        (handle != 0).then(|| Spectrum { handle })
    }
    #[cfg(not(target_os = "macos"))]
    None
}

/// Starts the CoreAudio global-output spectrum after a host app confirms
/// durable user consent from its persisted preference. Hosts must not call
/// this for first-run or revoked access: macOS may present system-audio consent
/// UI when that assertion is stale.
#[cfg(feature = "spectrum")]
pub fn spectrum_start_with_existing_consent() -> Option<Spectrum> {
    #[cfg(target_os = "macos")]
    {
        let handle = unsafe { sys::umr_spectrum_start_with_existing_consent() };
        (handle != 0).then(|| Spectrum { handle })
    }
    #[cfg(not(target_os = "macos"))]
    None
}

// MARK: - Subscription

type NowPlayingCallback = std::sync::Arc<dyn Fn(Option<NowPlaying>) + Send + Sync>;

static SUBSCRIPTIONS: Mutex<Option<std::collections::HashMap<u64, NowPlayingCallback>>> =
    Mutex::new(None);

/// Registry key for active subscriptions; also passed to the native layer as
/// the callback context so the trampoline can find its closure.
static NEXT_SUBSCRIPTION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

unsafe extern "C" fn subscription_trampoline(context: u64, json: *const c_char) {
    let callback = {
        let guard = SUBSCRIPTIONS
            .lock()
            .expect("subscription registry poisoned");
        let callback = guard.as_ref().and_then(|map| map.get(&context)).cloned();
        drop(guard);
        callback
    };
    let Some(callback) = callback else { return };
    // Deliver payload changes; NULL JSON means "nothing playing" and is
    // forwarded as `None`. Unparseable payloads are ignored.
    let snapshot = if json.is_null() {
        None
    } else {
        parse_json_c_string(json)
    };
    if json.is_null() || snapshot.is_some() {
        callback(snapshot);
    }
}

/// Active now-playing subscription. Dropping the value unsubscribes.
pub struct Subscription {
    /// Native handle used to unsubscribe on drop.
    handle: u64,
    /// Registry key matching the native callback context.
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe { sys::umr_now_playing_unsubscribe(self.handle) };
        if let Some(map) = SUBSCRIPTIONS
            .lock()
            .expect("subscription registry poisoned")
            .as_mut()
        {
            map.remove(&self.id);
        }
    }
}

/// Subscribes to Now Playing updates, delivered on a dedicated thread roughly
/// every `interval`. The callback receives `Some(snapshot)` when the payload
/// changed and `None` when nothing is playing anymore. Returns `None` when the
/// subscription could not be established.
///
/// This is a polling implementation over [`now_playing_fetch`]; intervals
/// below ~50 ms are clamped by the native layer.
pub fn now_playing_subscribe(
    interval: Duration,
    callback: impl Fn(Option<NowPlaying>) + Send + Sync + 'static,
) -> Option<Subscription> {
    let id = NEXT_SUBSCRIPTION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let handle = unsafe {
        sys::umr_now_playing_subscribe(Some(subscription_trampoline), id, timeout_ms(interval))
    };
    if handle == 0 {
        return None;
    }
    SUBSCRIPTIONS
        .lock()
        .expect("subscription registry poisoned")
        .get_or_insert_with(Default::default)
        .insert(id, std::sync::Arc::new(callback));
    Some(Subscription { handle, id })
}

// MARK: - Tests

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_volume_validation_rejects_out_of_bounds_values() {
        assert!(valid_output_volume(0.0));
        assert!(valid_output_volume(1.0));
        assert!(!valid_output_volume(-f64::EPSILON));
        assert!(!valid_output_volume(1.0 + f64::EPSILON));
        assert!(!valid_output_volume(f64::NAN));
        assert!(!valid_output_volume(f64::INFINITY));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_volume_stubs_are_unavailable() {
        assert_eq!(output_volume(), None);
        assert!(!set_output_volume(0.5));
    }

    #[test]
    fn media_snapshot_composes_independent_sources() {
        let now_playing = parse_now_playing(&json!({ "title": "Track" }));
        let capabilities = Capabilities {
            play_pause: true,
            previous: false,
            next: true,
            like: false,
            dislike: false,
        };
        let snapshot = compose_media_snapshot(now_playing.clone(), capabilities, Some(0.75));
        assert_eq!(snapshot.now_playing, now_playing);
        assert_eq!(snapshot.capabilities, capabilities);
        assert_eq!(snapshot.output_volume, Some(0.75));
    }

    #[test]
    fn transport_command_codes_match_media_remote_values() {
        assert_eq!(TransportCommand::PlayPause.code(), 2);
        assert_eq!(TransportCommand::Next.code(), 4);
        assert_eq!(TransportCommand::Previous.code(), 5);
        assert_eq!(TransportCommand::Like.code(), 106);
        assert_eq!(TransportCommand::Dislike.code(), 107);
    }

    #[test]
    fn transport_command_roundtrips_through_code() {
        for command in [
            TransportCommand::PlayPause,
            TransportCommand::Next,
            TransportCommand::Previous,
            TransportCommand::Like,
            TransportCommand::Dislike,
        ] {
            assert_eq!(TransportCommand::try_from(command.code()), Ok(command));
        }
        assert_eq!(TransportCommand::try_from(0), Err(UnknownCommand(0)));
        assert_eq!(TransportCommand::try_from(3), Err(UnknownCommand(3)));
        assert_eq!(
            TransportCommand::try_from(u32::MAX),
            Err(UnknownCommand(u32::MAX))
        );
    }

    #[test]
    fn capabilities_require_transport_availability() {
        let none = resolve_capabilities(false, &[2, 4, 5], false);
        assert!(!none.play_pause);
        assert!(!none.previous);
        assert!(!none.next);
        assert!(!none.like);
        assert!(!none.dislike);

        let send_only = resolve_capabilities(true, &[], false);
        assert!(send_only.play_pause);
        assert!(!send_only.previous);
        assert!(!send_only.next);

        let adapter = resolve_capabilities(true, &[], true);
        assert!(adapter.play_pause);
        assert!(adapter.previous);
        assert!(adapter.next);
        assert!(!adapter.like);
        assert!(!adapter.dislike);
    }

    #[test]
    fn capabilities_honor_enabled_command_codes() {
        // Codes are zero-based MRMediaRemoteCommand values: play/pause=2,
        // next=4, previous=5. The Swift layer only reports supported AND
        // enabled commands, so presence in the set is authoritative.
        let caps = resolve_capabilities(true, &[2, 4], false);
        assert!(caps.play_pause);
        assert!(caps.next);
        assert!(!caps.previous);
        assert!(!caps.like);
        assert!(!caps.dislike);

        let caps = resolve_capabilities(true, &[2, 5, 106, 107], false);
        assert!(caps.previous);
        assert!(!caps.next);
        assert!(caps.like);
        assert!(caps.dislike);

        // An empty direct discovery result must not invent secondary capabilities.
        let caps = resolve_capabilities(true, &[2], false);
        assert_eq!(
            caps,
            Capabilities {
                play_pause: true,
                previous: false,
                next: false,
                like: false,
                dislike: false,
            }
        );
    }

    #[test]
    fn now_playing_parses_full_payload() {
        let np = parse_now_playing(&json!({
            "title": "Song",
            "artist": "Artist",
            "album": "Album",
            "app_name": "Music",
            "bundle_id": "com.apple.Music",
            "unique_identifier": "dQw4w9WgXcQ",
            "content_item_identifier": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "media_type": 2,
            "pid": 1234,
            "elapsed_seconds": 42.5,
            "duration_seconds": 200.0,
            "is_playing": true,
        }))
        .expect("full payload parses");
        assert_eq!(np.title.as_deref(), Some("Song"));
        assert_eq!(np.artist.as_deref(), Some("Artist"));
        assert_eq!(np.album.as_deref(), Some("Album"));
        assert_eq!(np.app_name.as_deref(), Some("Music"));
        assert_eq!(np.bundle_id.as_deref(), Some("com.apple.Music"));
        assert_eq!(np.unique_identifier.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(
            np.content_item_identifier.as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert_eq!(np.media_type, Some(2));
        assert_eq!(np.pid, Some(1234));
        assert_eq!(np.elapsed_seconds, Some(42.5));
        assert_eq!(np.duration_seconds, Some(200.0));
        assert_eq!(np.is_playing, Some(true));

        // Serialization keeps snake_case field names.
        let serialized = serde_json::to_value(&np).unwrap();
        assert_eq!(serialized["app_name"], "Music");
        assert_eq!(serialized["is_playing"], true);
    }

    #[test]
    fn youtube_id_requires_exact_identifiers_or_explicit_urls() {
        assert_eq!(
            youtube_video_id(Some("dQw4w9WgXcQ"), None).as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            youtube_video_id(None, Some("https://youtu.be/dQw4w9WgXcQ?t=4")).as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            youtube_video_id(None, Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")).as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            youtube_video_id(Some("dQw4w9WgXcQ"), Some("https://youtu.be/aqz-KE-bpKQ"),),
            None,
        );
        assert_eq!(youtube_video_id(Some("UUID-dQw4w9WgXcQ"), None), None);
        assert_eq!(youtube_video_id(Some("A video title"), None), None);
        assert_eq!(
            youtube_video_id_from_url("https://example.com/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(
            youtube_video_id_from_url("https://www.youtube.com/watch?v=too-short"),
            None
        );
        assert_eq!(youtube_video_id(Some(" dQw4w9WgXcQ "), None), None);
    }

    #[test]
    fn now_playing_field_mapping_drops_invalid_values() {
        let np = parse_now_playing(&json!({
            "title": "Only Title",
            "pid": 0,
            "elapsed_seconds": -1.0,
            "duration_seconds": "not-a-number",
            "is_playing": null,
        }))
        .expect("payload with partial data parses");
        assert_eq!(np.title.as_deref(), Some("Only Title"));
        assert_eq!(np.pid, None, "non-positive PIDs are dropped");
        assert_eq!(np.elapsed_seconds, None, "negative times are dropped");
        assert_eq!(np.duration_seconds, None);
        assert_eq!(np.is_playing, None);
    }

    #[test]
    fn now_playing_rejects_non_objects() {
        assert!(parse_now_playing(&json!("nope")).is_none());
        assert!(parse_now_playing(&json!(42)).is_none());
        assert!(parse_now_playing(&json!(null)).is_none());
    }

    #[test]
    fn now_playing_serialization_omits_missing_optional_times() {
        let np = parse_now_playing(&json!({ "title": "X", "is_playing": false })).unwrap();
        let serialized = serde_json::to_string(&np).unwrap();
        assert!(serialized.contains("\"title\":\"X\""));
        assert!(!serialized.contains("elapsed_seconds"));
        assert!(!serialized.contains("duration_seconds"));
        assert!(serialized.contains("\"is_playing\":false"));
    }
    #[test]
    fn transport_falls_back_after_adapter_failure() {
        assert!(resolve_transport_send(Some(true), || {
            panic!("direct send must not run after adapter success")
        }));
        assert!(resolve_transport_send(Some(false), || true));
        assert!(resolve_transport_send(None, || true));
        assert!(!resolve_transport_send(Some(false), || false));
    }

    #[test]
    fn seek_positions_convert_to_adapter_microseconds() {
        assert_eq!(seek_position_micros(0.0), Some(0));
        assert_eq!(seek_position_micros(1.25), Some(1_250_000));
        assert_eq!(seek_position_micros(-0.1), None);
        assert_eq!(seek_position_micros(f64::NAN), None);
        assert_eq!(seek_position_micros(f64::INFINITY), None);
        assert_eq!(seek_position_micros(i64::MAX as f64), None);
    }

    #[test]
    fn adapter_payload_parses_realistic_sample() {
        let np = parse_adapter_now_playing(&json!({
            "title": "Bohemian Rhapsody",
            "artworkData": "QUJD",
            "artworkMimeType": "image/jpeg",
            "artist": "Queen",
            "album": "A Night At The Opera",
            "applicationName": "Music",
            "bundleIdentifier": "com.apple.Music",
            "uniqueIdentifier": "dQw4w9WgXcQ",
            "contentItemIdentifier": "https://youtu.be/dQw4w9WgXcQ",
            "mediaType": 2,
            "processIdentifier": 412,
            "elapsedTimeNow": 83.2,
            "duration": 354.32,
            "playing": true,
            "playbackRate": 1.0,
        }))
        .expect("realistic adapter payload parses");
        assert_eq!(np.title.as_deref(), Some("Bohemian Rhapsody"));
        assert_eq!(np.artist.as_deref(), Some("Queen"));
        assert_eq!(np.album.as_deref(), Some("A Night At The Opera"));
        assert_eq!(np.app_name.as_deref(), Some("Music"));
        assert_eq!(np.bundle_id.as_deref(), Some("com.apple.Music"));
        assert_eq!(np.unique_identifier.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(
            np.content_item_identifier.as_deref(),
            Some("https://youtu.be/dQw4w9WgXcQ")
        );
        assert_eq!(np.media_type, Some(2));
        assert_eq!(np.pid, Some(412));
        assert_eq!(np.elapsed_seconds, Some(83.2));
        assert_eq!(np.duration_seconds, Some(354.32));
        assert_eq!(np.is_playing, Some(true));

        assert_eq!(
            np.artwork_data_url.as_deref(),
            Some("data:image/jpeg;base64,QUJD"),
        );
    }

    #[test]
    fn adapter_playing_state_falls_back_to_playback_rate() {
        let np = parse_adapter_now_playing(&json!({
            "title": "Paused Track",
            "playbackRate": 0.0,
        }))
        .expect("rate-only payload parses");
        assert_eq!(np.is_playing, Some(false));
        assert_eq!(np.elapsed_seconds, None, "no elapsedTime keys present");
    }

    #[test]
    fn adapter_payload_rejects_empty_metadata_and_bad_values() {
        // No metadata keys at all: an empty session must not shadow the
        // direct MediaRemote fallback.
        assert!(parse_adapter_now_playing(&json!({ "playing": false })).is_none());
        assert!(parse_adapter_now_playing(&json!({})).is_none());

        // Whitespace-only strings are dropped; invalid PID/time values too.
        let np = parse_adapter_now_playing(&json!({
            "title": "  ",
            "artist": "Artist",
            "album": "",
            "processIdentifier": -5,
            "elapsedTime": -1.0,
        }))
        .expect("artist-only payload parses");
        assert_eq!(np.title, None);
        assert_eq!(np.album, None);
        assert_eq!(np.artist.as_deref(), Some("Artist"));
        assert_eq!(np.pid, None);
        assert_eq!(np.elapsed_seconds, None);
    }
    #[test]
    fn adapter_artwork_is_bounded_and_mime_checked() {
        let base = json!({ "title": "T" });

        // Non-image MIME types are rejected.
        let np = parse_adapter_now_playing(&serde_json::json!({
            "title": "T",
            "artworkData": "QUJD",
            "artworkMimeType": "text/plain",
        }))
        .unwrap();
        assert_eq!(np.artwork_data_url, None);

        // Artwork over the 1 MiB base64 cap is dropped, payload still parses.
        let oversized = "A".repeat(1024 * 1024 + 1);
        let np = parse_adapter_now_playing(&serde_json::json!({
            "title": "T",
            "artworkData": oversized,
            "artworkMimeType": "image/png",
        }))
        .unwrap();
        assert_eq!(np.artwork_data_url, None);

        // Missing artwork keys leave the field empty without error.
        let np = parse_adapter_now_playing(&base).unwrap();
        assert_eq!(np.artwork_data_url, None);
    }
    #[cfg(all(feature = "spectrum", not(target_os = "macos")))]
    #[test]
    fn non_macos_spectrum_preflight_and_start_are_unavailable() {
        assert!(!spectrum_permission_granted());
        assert!(!spectrum_request_permission());
        assert!(spectrum_start().is_none());
    }
    #[cfg(feature = "spectrum")]
    #[test]
    fn spectrum_payload_parses_and_clamps() {
        let good = "[0.0, 0.25, 1.0, 0.5, 0.75, 0.1, 0.2, 0.3, 0.4, 0.9, 1.0]";
        let parsed = parse_spectrum(good).expect("11-band payload parses");
        assert_eq!(parsed.len(), SPECTRUM_BAND_FREQUENCIES_HZ.len());
        assert_eq!(parsed[2], 1.0);

        // Out-of-range values clamp; wrong lengths are rejected.
        let clamped = parse_spectrum(&serde_json::json!([]).to_string());
        assert!(clamped.is_none());
        let bad = parse_spectrum("[0, 0, 0]");
        assert!(bad.is_none(), "wrong band count must be rejected");
        assert!(parse_spectrum("not json").is_none());
    }
}
