# ultra-media-remote

**Read macOS “Now Playing” info, artwork, and transport state from a safe Rust API without statically linking Apple's private framework.**

Every macOS system ships a canonical media session for Music, Spotify, QuickTime, browsers, and other players. `ultra-media-remote` uses two runtime paths: a staged BSD-licensed MediaRemoteAdapter launched through `/usr/bin/perl` for complete modern-macOS metadata, and a small Swift `dlopen`/`dlsym` shim as the direct fallback and transport layer. Missing frameworks, symbols, adapter files, or sessions degrade to `None`/`false` instead of crashing or fabricating data.

## Features

- **Reusable media snapshot**: `media_snapshot(timeout)` composes Now Playing, transport capabilities, and default-output volume without app-specific player assumptions.
- **Now Playing snapshot**: title, artist, album, artwork, owning app name/bundle ID/PID, elapsed/duration seconds, and playing state as a serde-serializable struct.
- **Transport control**: play/pause, next, previous — sent only when the system reports them as available.
- **Capability discovery**: per-command support/enabled state via MediaRemote's command-info APIs.
- **Output volume**: read or set the default CoreAudio output scalar in the normalized range [0, 1].
- **Live updates** (optional): poll-based subscription delivering snapshots when they change.
- **Graceful degradation**: unavailable frameworks yield `None`/`false`, never fake data. Non-macOS targets compile to stubs.

## Requirements

- macOS 14+ (arm64). The Swift toolchain (`swift build`) must be installed; the crate builds its Swift component automatically through `build.rs`.
- On non-macOS hosts the crate compiles, but every query reports unavailable.
- **macOS 15.4+ metadata requires the staged adapter and a Developer ID signature.** An ad-hoc-signed `MediaRemoteAdapter.framework` loads, but MediaRemote returns empty sessions. See [Staging the adapter](#staging-the-adapter-macos-154-metadata).

## Install

```toml
[dependencies]
ultra-media-remote = "0.1"

# Optional: live system-output spectrum (11-band EQ data).
ultra-media-remote = { version = "0.1", features = ["spectrum"] }
```

## Usage

```rust
use std::time::Duration;
use ultra_media_remote::{
    media_snapshot, now_playing_fetch, output_volume, set_output_volume,
    transport_capabilities, transport_send, TransportCommand,
};

// Current track
if let Some(np) = now_playing_fetch(Duration::from_secs(1)) {
    println!("{} — {} ({:?})", np.title.as_deref().unwrap_or("?"), np.artist.as_deref().unwrap_or("?"), np.app_name);
}

// Transport
let caps = transport_capabilities();
if caps.next {
    transport_send(TransportCommand::Next);
}

// Reusable combined state and default-output volume
let media = media_snapshot(Duration::from_secs(1));
if let Some(volume) = media.output_volume {
    println!("system output volume: {volume:.2}");
}
if let Some(volume) = output_volume() {
    let _ = set_output_volume(volume);
}
let _sub = ultra_media_remote::now_playing_subscribe(Duration::from_millis(500), |update| {
    // Some(snapshot) on change, None when nothing is playing anymore.
});
```

### Live spectrum (feature `spectrum`)

```rust
// Run the request call only from an explicit user action.
if !ultra_media_remote::spectrum_permission_granted()
    && !ultra_media_remote::spectrum_request_permission()
{
    return;
}
let bands = ultra_media_remote::spectrum_start()?; // macOS 15+, access granted
loop {
    if let Some(levels) = bands.fetch() {
        // 11 normalized levels [0, 1] for 63 Hz .. 14 kHz, ready to render.
    }
}
```

### Example: print Now Playing JSON

```console
$ cargo run --example nowplaying
```

A live macOS 26 verification with **“Slip” by Autechre** returned real track metadata and cover art. The base64 payload is shortened here only for readability:

```json
{
  "title": "Slip",
  "artist": "Autechre",
  "artwork_data_url": "data:image/jpeg;base64,<artwork omitted>"
}
```

Fields are optional: players omit what they do not report, and unknown values are dropped rather than guessed. When nothing plays, the example prints an honest “no now-playing information” message. Transport capabilities reflect the active system session.

> **Platform note:** direct `MediaRemote.framework` access through `dlopen` remains available as a fallback, but Apple restricts direct `MRMediaRemoteGetNowPlayingInfo` replies on macOS 15.4 and later. The crate therefore tries the bundled BSD-licensed [MediaRemoteAdapter](third_party/mediaremote-adapter) first. Its `/usr/bin/perl` launcher supplies the system entitlement required by modern macOS, allowing `now_playing_fetch` to return full metadata, including artwork, when both adapter files are staged correctly.
>
> **A Developer ID signature on `MediaRemoteAdapter.framework` is mandatory for modern-macOS metadata.** Ad-hoc signing is insufficient: the framework loads, but queries return empty sessions. If the adapter is absent or returns no usable session, the crate falls back to direct runtime MediaRemote access and still reports `None` rather than fabricating data.

## Trust & security notes

- This crate talks to **`MediaRemote.framework`, a private Apple framework**, located at `/System/Library/PrivateFrameworks/MediaRemote.framework`. It is loaded at runtime with `dlopen` and accessed exclusively through `dlsym`-resolved C entry points; nothing from the framework is statically linked into your binary.
- Because the framework is private, Apple may remove or change symbols in any macOS release. The design assumption is that missing pieces disable features individually — availability checks precede every operation, and failures surface as `false`/`None`, never as fabricated results or crashes.
- Reading Now Playing metadata and sending transport commands does not require special TCC permissions today, but treat that as an implementation detail of macOS, not a guarantee. Apps distributed through the Mac App Store should not rely on private frameworks.
- The Swift component only uses public AppKit/Foundation APIs besides the dlopen'd MediaRemote symbols (for example, `NSRunningApplication` to resolve the owning app's name and bundle ID).
- The BSD-licensed adapter is launched only for metadata reads through `/usr/bin/perl`. Its stdout is size-bounded and parsed as untrusted JSON; artwork must declare an `image/` MIME type and stay within the crate's 1 MiB base64 limit.

## Staging the adapter (macOS 15.4+ metadata and transport)

Transport delivery also prefers the staged MediaRemoteAdapter (`send COMMAND` with the same zero-based command IDs), falling back to the dlopen'd send API; `play_pause` capability is reported when either send path exists.

The adapter has two parts that must ship with your app:

- `mediaremote-adapter.pl` — the loader script
- `MediaRemoteAdapter.framework/` — a dylib framework built from `third_party/mediaremote-adapter`

Build the framework once from the vendored source (requires cmake + Xcode toolchain):

```console
cmake -S third_party/mediaremote-adapter -B build/adapter -DCMAKE_BUILD_TYPE=Release
cmake --build build/adapter
# -> build/adapter/MediaRemoteAdapter.framework
```

Then sign the framework. On macOS 15.4+ (observed on macOS 26) MediaRemote only serves Now Playing data when the loaded dylib carries a valid code signature; an ad-hoc signature loads fine but every query returns an empty session:


> **Do not use `codesign --sign -` for a release or metadata test.** Ad-hoc signing produces empty Now Playing sessions on modern macOS even though the framework loads successfully.

```console
codesign --force --options runtime \
  --sign "Developer ID Application: <Your Team Name> (<Team ID>)" \
  build/adapter/MediaRemoteAdapter.framework
```

Stage both parts next to your binary, or in your app bundle's Resources:

```text
myapp                          # your binary
mediaremote/
  mediaremote-adapter.pl
  MediaRemoteAdapter.framework/

# or inside an app bundle:
MyApp.app/Contents/Resources/mediaremote/
  mediaremote-adapter.pl
  MediaRemoteAdapter.framework/
```

Discovery order: the `ULTRA_MEDIA_REMOTE_ADAPTER_DIR` environment variable overrides everything; otherwise `<exe dir>/mediaremote`, then `<exe dir>/../Resources/mediaremote`. A directory counts only when it contains both files. When the adapter is not found or reports no session, [`now_playing_fetch`] silently falls back to the direct MediaRemote path.

## Spectrum feature notes

- The spectrum is an opt-in cargo feature. Without it, no ScreenCaptureKit code is compiled into your binary at all.
- It captures **system output audio** through a ScreenCaptureKit audio tap (`capturesAudio`, microphone excluded, the calling process's own audio excluded), then analyzes fixed frequency bins with Goertzel magnitudes — identical math to the reference implementation this crate was extracted from, so visuals match.
- **Permission:** ScreenCaptureKit requires **Screen Recording** permission (System Settings → Privacy & Security → Screen Recording) even for audio-only taps. Call `spectrum_permission_granted()` for a non-prompting preflight. `spectrum_start()` returns `None` until access is already granted and never requests it. Call `spectrum_request_permission()` only from an explicit consent action when your app wants macOS to show the system prompt.
- Runtime requirement: macOS 15 or newer; on older systems `spectrum_start` returns `None`.

## Development

```console
cargo build                        # compiles the Swift package, links it, builds the crate
cargo test                         # Rust unit tests for pure logic
cd native/UltraMediaRemote && swift test -Xswiftc -DUMR_SPECTRUM   # spectrum analysis tests
cargo run --example nowplaying
```

Layout:

- `native/UltraMediaRemote/` — Swift package producing `libUltraMediaRemote.a` with the `umr_*` C ABI (`dlopen` controller, thread-safe handoff boxes, polling subscriptions).
- `src/lib.rs` — FFI declarations, safe wrappers, pure mapping logic and tests.
- `build.rs` — invokes `swift build -c debug|release` matching the Cargo profile and wires up link flags (including `/usr/lib/swift` rpath for the concurrency runtime).
- `third_party/mediaremote-adapter/` — vendored BSD-3-Clause adapter providing the macOS 15.4+ metadata path.

## License

MIT © 2026 Implose Cybernetics. See [LICENSE](LICENSE).

## Third-party notices

[MediaRemoteAdapter](https://github.com/ungive/mediaremote-adapter) is vendored under `third_party/mediaremote-adapter` and redistributed under its BSD 3-Clause license (© Jonas van den Berg and contributors); see [third_party/mediaremote-adapter/LICENSE](third_party/mediaremote-adapter/LICENSE). It is invoked at runtime through `/usr/bin/perl` and is not linked into binaries built against this crate.
