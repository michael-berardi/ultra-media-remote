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
use std::sync::Mutex;
use std::time::Duration;

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
}

/// Transport commands accepted by [`transport_send`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportCommand {
    PlayPause,
    Next,
    Previous,
}

impl TransportCommand {
    /// Zero-based `MRMediaRemoteCommand` value sent to MediaRemote.
    pub const fn code(self) -> u32 {
        match self {
            TransportCommand::PlayPause => 2,
            TransportCommand::Next => 4,
            TransportCommand::Previous => 5,
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
        pid: object
            .get("pid")
            .and_then(|v| v.as_i64())
            .filter(|pid| *pid > 0 && *pid <= i32::MAX as i64)
            .map(|pid| pid as i32),
        elapsed_seconds: time(object, "elapsed_seconds"),
        duration_seconds: time(object, "duration_seconds"),
        is_playing: object.get("is_playing").and_then(|v| v.as_bool()),
    })
}

/// Resolves transport capabilities from transport availability and the set of
/// enabled+supported command codes discovered by MediaRemote.
pub(crate) fn resolve_capabilities(send_available: bool, enabled_codes: &[u32]) -> Capabilities {
    Capabilities {
        play_pause: send_available,
        previous: send_available && enabled_codes.contains(&TransportCommand::Previous.code()),
        next: send_available && enabled_codes.contains(&TransportCommand::Next.code()),
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
        pub fn umr_now_playing_fetch(timeout_ms: u32) -> *mut c_char;
        pub fn umr_free_string(s: *mut c_char);
        pub fn umr_transport_supported_commands(codes: *mut u32, capacity: u32) -> c_int;
        pub fn umr_transport_send(code: u32) -> c_int;
        pub fn umr_now_playing_subscribe(
            callback: Option<SubscribeCallback>,
            context: u64,
            interval_ms: u32,
        ) -> u64;
        pub fn umr_now_playing_unsubscribe(handle: u64);
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
    pub unsafe fn umr_now_playing_subscribe(
        _callback: Option<unsafe extern "C" fn(u64, *const c_char)>,
        _context: u64,
        _interval_ms: u32,
    ) -> u64 {
        0
    }
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

/// Fetches the current Now Playing snapshot. Returns `None` when MediaRemote
/// is unavailable, nothing is playing, or the reply did not arrive within
/// `timeout`.
pub fn now_playing_fetch(timeout: Duration) -> Option<NowPlaying> {
    let pointer = unsafe { sys::umr_now_playing_fetch(timeout_ms(timeout)) };
    if pointer.is_null() {
        return None;
    }
    let parsed = parse_json_c_string(pointer);
    unsafe { sys::umr_free_string(pointer) };
    parsed
}

/// Reads transport capabilities for the current system media session. Missing
/// command-discovery APIs safely report `previous`/`next` as `false`;
/// `play_pause` remains available whenever the send API resolved.
pub fn transport_capabilities() -> Capabilities {
    let mut codes = [0u32; COMMAND_CODE_CAPACITY as usize];
    let count = unsafe { sys::umr_transport_supported_commands(codes.as_mut_ptr(), codes.len() as u32) };
    let count = count.clamp(0, codes.len() as i32) as usize;
    resolve_capabilities(transport_available(), &codes[..count])
}

/// Sends a transport command through runtime MediaRemote access. Returns
/// `false` when the transport is unavailable or delivery failed.
pub fn transport_send(command: TransportCommand) -> bool {
    unsafe { sys::umr_transport_send(command.code()) != 0 }
}

// MARK: - Subscription

type NowPlayingCallback = std::sync::Arc<dyn Fn(Option<NowPlaying>) + Send + Sync>;

static SUBSCRIPTIONS: Mutex<Option<std::collections::HashMap<u64, NowPlayingCallback>>> =
    Mutex::new(None);

/// Registry key for active subscriptions; also passed to the native layer as
/// the callback context so the trampoline can find its closure.
static NEXT_SUBSCRIPTION_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

unsafe extern "C" fn subscription_trampoline(context: u64, json: *const c_char) {
    let callback = {
        let guard = SUBSCRIPTIONS.lock().expect("subscription registry poisoned");
        let callback = guard
            .as_ref()
            .and_then(|map| map.get(&context))
            .cloned();
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
    fn transport_command_codes_match_media_remote_values() {
        assert_eq!(TransportCommand::PlayPause.code(), 2);
        assert_eq!(TransportCommand::Next.code(), 4);
        assert_eq!(TransportCommand::Previous.code(), 5);
    }

    #[test]
    fn transport_command_roundtrips_through_code() {
        for command in [
            TransportCommand::PlayPause,
            TransportCommand::Next,
            TransportCommand::Previous,
        ] {
            assert_eq!(TransportCommand::try_from(command.code()), Ok(command));
        }
        assert_eq!(TransportCommand::try_from(0), Err(UnknownCommand(0)));
        assert_eq!(TransportCommand::try_from(3), Err(UnknownCommand(3)));
        assert_eq!(TransportCommand::try_from(u32::MAX), Err(UnknownCommand(u32::MAX)));
    }

    #[test]
    fn capabilities_require_transport_availability() {
        let none = resolve_capabilities(false, &[2, 4, 5]);
        assert!(!none.play_pause);
        assert!(!none.previous);
        assert!(!none.next);

        let send_only = resolve_capabilities(true, &[]);
        assert!(send_only.play_pause);
        assert!(!send_only.previous);
        assert!(!send_only.next);
    }

    #[test]
    fn capabilities_honor_enabled_command_codes() {
        // Codes are zero-based MRMediaRemoteCommand values: play/pause=2,
        // next=4, previous=5. The Swift layer only reports supported AND
        // enabled commands, so presence in the set is authoritative.
        let caps = resolve_capabilities(true, &[2, 4]);
        assert!(caps.play_pause);
        assert!(caps.next);
        assert!(!caps.previous);

        let caps = resolve_capabilities(true, &[2, 5]);
        assert!(caps.previous);
        assert!(!caps.next);

        // An empty discovery result must not invent secondary capabilities.
        let caps = resolve_capabilities(true, &[2]);
        assert_eq!(
            caps,
            Capabilities { play_pause: true, previous: false, next: false }
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
}
