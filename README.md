# ultra-media-remote

Read macOS Now Playing metadata and control playback from Rust. For desktop
apps that need track details, artwork, transport controls, or output volume
without separate integrations for each player.

Requires macOS 14+ and Rust plus the Swift toolchain. Builds are verified on
Apple silicon. The Swift bridge loads Apple's private MediaRemote framework
at runtime; it is not a Mac App Store API. Non-macOS builds use native stubs.

## Install

From your Rust project:

```sh
cargo add ultra-media-remote --git https://github.com/michael-berardi/ultra-media-remote --tag v0.2.7
```

Cargo builds and links the Swift component automatically. Releases currently
contain source, not prebuilt libraries or installers.

**On macOS 15.4+, full metadata needs a separately staged, Developer ID-signed
adapter.** Installing the crate alone does not provide it. See
[Adapter setup](#adapter-setup).

## First snapshot

```rust
use std::time::Duration;
use ultra_media_remote::media_snapshot;

fn main() {
    let snapshot = media_snapshot(Duration::from_secs(1));
    if let Some(track) = snapshot.now_playing {
        println!("{}", track.title.as_deref().unwrap_or("Unknown title"));
    } else {
        println!("No metadata available");
    }
}
```

From a checkout, `cargo run --example nowplaying` prints availability,
metadata JSON when available, and transport capabilities. Optional fields
remain absent when the player supplies no value.

## API and limits

- `now_playing_fetch(timeout)` reads title, artist, album, artwork, owning app,
  playback timing, and identifiers. Adapter results are cached for two seconds.
  The timeout covers only the direct Swift query, not the adapter subprocess.
- `media_snapshot(timeout)` combines metadata, transport capabilities, and
  default-output volume. These are separate queries, not an atomic snapshot.
- `transport_send(TransportCommand::PlayPause)` also supports `Next`,
  `Previous`, `Like`, and `Dislike`. `seek(seconds)` uses an absolute position.
  Both prefer the adapter, then fall back to direct MediaRemote access.
- `transport_capabilities()` uses supported/enabled command information when
  available. A staged adapter enables the standard play/pause, next, and
  previous flags without checking the player's support. Rating flags require
  advertised commands and an available send path. Delivery can still fail.
- `output_volume()` and `set_output_volume(value)` use the default CoreAudio
  output device; values are in `[0, 1]`. Devices without scalar controls may
  report unavailable.
- `now_playing_subscribe(interval, callback)` polls the **direct Swift path**,
  not the adapter. It may therefore miss modern-macOS sessions that a fetch
  can read. Dropping its handle unsubscribes; intervals below 50 ms are clamped.
- YouTube IDs are extracted from explicit metadata identifiers; conflicts are
  rejected. External video-window capture is not provided.

Unavailable operations normally return `None` or `false`. Private framework
symbols and OS entitlement rules can change; successful loading does not
promise a usable media session. Do not run blocking fetches on your UI thread.

## Adapter setup

The vendored [MediaRemoteAdapter](third_party/mediaremote-adapter) is launched
through `/usr/bin/perl` for metadata **and transport**. Its system entitlement
provides the modern-macOS metadata path. Build with CMake and the Xcode toolchain:

```sh
cmake -S third_party/mediaremote-adapter -B build/adapter -DCMAKE_BUILD_TYPE=Release
cmake --build build/adapter --parallel 4
```

CMake ad-hoc signs the framework automatically. For distribution, replace that
signature with your Developer ID signature:

```sh
codesign --force --options runtime \
  --sign "Developer ID Application: <Your Team Name> (<Team ID>)" \
  build/adapter/MediaRemoteAdapter.framework
```

The existing macOS 26 verification found empty sessions with an ad-hoc
signature; use a valid Developer ID signature for modern-macOS metadata.
Signing does not guarantee compatibility with future OS versions.

Ship these two items in a `mediaremote` directory next to your executable,
or in `MyApp.app/Contents/Resources/mediaremote/`:

- `third_party/mediaremote-adapter/bin/mediaremote-adapter.pl`
- `build/adapter/MediaRemoteAdapter.framework/`

Discovery checks `ULTRA_MEDIA_REMOTE_ADAPTER_DIR`, then
`<exe dir>/mediaremote`, then `<exe dir>/../Resources/mediaremote`. It uses the
first directory containing both items; an invalid environment override does
not disable the other candidates. Missing or unusable adapter metadata falls
back to the direct path.

Only stage trusted adapter files. Adapter JSON is treated as untrusted input:
stdout is capped at 1.5 MiB, and artwork needs an `image/` MIME type with at
most 1 MiB of base64 data. Adapter subprocesses have no wall-clock deadline.

## Optional spectrum

Enable the Cargo feature `spectrum` for 11 normalized output-audio bands from
63 Hz to 14 kHz. It requires macOS 15+. Without the feature, the
ScreenCaptureKit implementation is not compiled.

`spectrum_permission_granted()` checks Screen Recording access without a
prompt. Call `spectrum_request_permission()` only from an explicit user action.
`spectrum_start()` returns `None` until access is granted and never requests it.
Keep the returned handle and call `fetch()` to read levels; dropping it stops
capture. The ScreenCaptureKit path excludes microphone and the caller's audio.

For hosts migrating persisted user consent,
`spectrum_start_with_existing_consent()` uses a CoreAudio global-output tap
instead. It does not perform the Screen Recording preflight; use it only for
previously granted consent, not to bypass first-run permission handling.
See [the spectrum example](examples/spectrum.rs).

## Development

```sh
cargo build -j4
cargo fmt --check
cargo clippy -j4 --all-targets -- -D warnings
cargo test -j4
(cd native/UltraMediaRemote && swift test -j 4 -Xswiftc -DUMR_SPECTRUM)
```

`src/lib.rs` contains Rust wrappers and mapping tests;
`native/UltraMediaRemote/` contains the Swift static library and analysis tests.
`build.rs` selects a separate Swift scratch directory for each profile/feature
variant; `ULTRA_MEDIA_REMOTE_SCRATCH` overrides its root. Cargo's job limit does
not propagate to the nested Swift build; resource-limited builders should
also constrain their Swift invocation.

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## License

MIT © 2026 Pareto Cybernetics. See [LICENSE](LICENSE).

## Third-party notices

[MediaRemoteAdapter](https://github.com/ungive/mediaremote-adapter) is vendored under `third_party/mediaremote-adapter` and redistributed under its BSD 3-Clause license (© Jonas van den Berg and contributors); see [third_party/mediaremote-adapter/LICENSE](third_party/mediaremote-adapter/LICENSE). It is invoked at runtime through `/usr/bin/perl` and is not linked into binaries built against this crate.

The CoreAudio process-tap lifecycle was informed by
[AudioCap](https://github.com/insidegui/AudioCap) (© 2024 Guilherme Rambo);
its permissive license is reproduced at
[`third_party/AudioCap-LICENSE`](third_party/AudioCap-LICENSE).
