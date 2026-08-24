# ultra-media-remote

**Read macOS "Now Playing" info and control media playback from Rust — no private headers, no fragile static linking.**

Every macOS system ships a canonical media session: whatever Music, Spotify, QuickTime, or a browser tab is playing right now. Apple exposes it through the private `MediaRemote` framework, which has no public headers and changes between OS releases. `ultra-media-remote` wraps that framework behind a small, proven Swift static library and a safe Rust API, resolving all symbols at runtime with `dlopen`/`dlsym`. If the framework or any symbol is missing, everything degrades gracefully to "unavailable" instead of crashing.

## Features

- **Now Playing snapshot**: title, artist, album, owning app name/bundle ID/PID, elapsed/duration seconds, and playing state as a serde-serializable struct.
- **Transport control**: play/pause, next, previous — sent only when the system reports them as available.
- **Capability discovery**: per-command support/enabled state via MediaRemote's command-info APIs.
- **Live updates** (optional): poll-based subscription delivering snapshots when they change.
- **Graceful degradation**: unavailable frameworks yield `None`/`false`, never fake data. Non-macOS targets compile to stubs.

## Requirements

- macOS 14+ (arm64). The Swift toolchain (`swift build`) must be installed; the crate builds its Swift component automatically through `build.rs`.
- On non-macOS hosts the crate compiles, but every query reports unavailable.

## Install

```toml
[dependencies]
ultra-media-remote = "0.1"
```

## Usage

```rust
use std::time::Duration;
use ultra_media_remote::{now_playing_fetch, transport_capabilities, transport_send, TransportCommand};

// Current track
if let Some(np) = now_playing_fetch(Duration::from_secs(1)) {
    println!("{} — {} ({:?})", np.title.as_deref().unwrap_or("?"), np.artist.as_deref().unwrap_or("?"), np.app_name);
}

// Transport
let caps = transport_capabilities();
if caps.next {
    transport_send(TransportCommand::Next);
}

let _sub = ultra_media_remote::now_playing_subscribe(Duration::from_millis(500), |update| {
    // Some(snapshot) on change, None when nothing is playing anymore.
});
```

### Example: print Now Playing JSON

```console
$ cargo run --example nowplaying
```

With Music playing, this prints something like:

```json
{
  "title": "Example Track",
  "artist": "Example Artist",
  "album": "Example Album",
  "app_name": "Music",
  "bundle_id": "com.apple.Music",
  "pid": 412,
  "elapsed_seconds": 83.0,
  "duration_seconds": 214.0,
  "is_playing": true
}
```

> Fields are optional: players omit what they do not report (browser tabs often withhold `pid`), and unknown values are dropped rather than guessed. When nothing plays, the example prints an honest "no now-playing information" message. Transport capabilities reflect your actual hardware/system state, so the trailing line may show fewer enabled commands than above.
>
> **Platform note:** on macOS 15.4 and later, Apple restricts `MRMediaRemoteGetNowPlayingInfo` responses to processes Apple considers eligible. On such systems the fetch can return `None` even while audio plays — as observed on macOS 26 during development — while the availability checks still succeed and transport control keeps working. The crate never fabricates metadata to cover this case.

## Trust & security notes

- This crate talks to **`MediaRemote.framework`, a private Apple framework**, located at `/System/Library/PrivateFrameworks/MediaRemote.framework`. It is loaded at runtime with `dlopen` and accessed exclusively through `dlsym`-resolved C entry points; nothing from the framework is statically linked into your binary.
- Because the framework is private, Apple may remove or change symbols in any macOS release. The design assumption is that missing pieces disable features individually — availability checks precede every operation, and failures surface as `false`/`None`, never as fabricated results or crashes.
- Reading Now Playing metadata and sending transport commands does not require special TCC permissions today, but treat that as an implementation detail of macOS, not a guarantee. Apps distributed through the Mac App Store should not rely on private frameworks.
- The Swift component only uses public AppKit/Foundation APIs besides the dlopen'd MediaRemote symbols (for example, `NSRunningApplication` to resolve the owning app's name and bundle ID).

## Development

```console
cargo build              # compiles the Swift package, links it, builds the crate
cargo test               # unit tests for pure logic (command codes, capability resolution, field mapping)
cargo run --example nowplaying
```

Layout:

- `native/UltraMediaRemote/` — Swift package producing `libUltraMediaRemote.a` with the `umr_*` C ABI (`dlopen` controller, thread-safe handoff boxes, polling subscriptions).
- `src/lib.rs` — FFI declarations, safe wrappers, pure mapping logic and tests.
- `build.rs` — invokes `swift build -c debug|release` matching the Cargo profile and wires up link flags (including `/usr/lib/swift` rpath for the concurrency runtime).

## License

MIT © 2026 Implose Cybernetics. See [LICENSE](LICENSE).
