# ultra-media-remote

Read macOS Now Playing metadata and control playback from Rust. For desktop
apps that need track details, artwork, transport controls, or output volume
without separate integrations for each player.

## Install

macOS 14+ on Apple silicon, with a signed, notarized and stapled CLI app bundle and staged adapter:

```sh
curl -fsSL https://github.com/michael-berardi/ultra-media-remote/releases/latest/download/install.sh | sh
```

The installer verifies `SHA256SUMS`, installs into
`~/.local/share/ultra-media-remote/<version>` and adds a shim at
`~/.local/bin/ultra-media-remote` (add that directory to `PATH`). It refuses to
replace a binary it did not install. Pin a version with
`ULTRA_MEDIA_REMOTE_VERSION=0.3.0`, or point at a local mirror with
`ULTRA_MEDIA_REMOTE_RELEASE_BASE`; `ULTRA_MEDIA_REMOTE_HOME` and
`ULTRA_MEDIA_REMOTE_BIN_DIR` change the destinations. Other platforms are
refused with a clear message.

From your Rust project, without the CLI:

```sh
cargo add ultra-media-remote --git https://github.com/michael-berardi/ultra-media-remote --tag v0.3.0
```

Requires macOS 14+ and Rust plus the Swift toolchain. Builds are verified on
Apple silicon. The Swift bridge loads Apple's private MediaRemote framework
at runtime; it is not a Mac App Store API. Non-macOS builds use native stubs.

Cargo builds and links the Swift component automatically. GitHub releases ship
a macOS arm64 tarball (signed CLI app bundle, staged adapter, licenses),
`install.sh`, and `SHA256SUMS`.

**On macOS 15.4+, full metadata needs a Developer ID-signed adapter.** The
installer stages it for the CLI; a Cargo-only build must stage it itself. See
[Adapter setup](#adapter-setup).

## Updates

The CLI checks for a newer release at most once a day and prints a short notice
after your command; `ultra-media-remote update` installs it, and automatic
installs are opt-in. The library never checks. See [docs/updates.md](docs/updates.md).

## Privacy

- No telemetry, analytics or identifiers. Track metadata stays on your Mac.
- The library makes no network requests and writes nothing to disk.
- The CLI's only request on its own is the daily update check: one anonymous GET of the public release record.
- It is off in CI, agent shells and non-terminal use, and with `ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK=1`.
- Details, local files and how to remove them: [PRIVACY.md](PRIVACY.md).

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

## CLI

Every command prints JSON on stdout and sends diagnostics to stderr:

```sh
ultra-media-remote now            # Now Playing object, or null when idle
ultra-media-remote snapshot       # Now Playing + capabilities + volume
ultra-media-remote caps           # transport capabilities
ultra-media-remote play-pause     # also: next, previous
ultra-media-remote seek 42        # absolute position in seconds
ultra-media-remote volume         # {"volume":number|null}
ultra-media-remote volume 0.5     # set default output volume
ultra-media-remote watch          # one JSON line each time Now Playing changes
ultra-media-remote --version      # ultra-media-remote <version>
ultra-media-remote --help
```

Transport commands print `{"command":"...","sent":true|false}` and exit 1
when delivery failed; invalid arguments exit 2. The CLI finds its adapter next
to the binary (`mediaremote/` or `lib/mediaremote/`), in an app bundle's
`Contents/Resources/mediaremote/`, or through
`ULTRA_MEDIA_REMOTE_ADAPTER_DIR`.

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
`<exe dir>/mediaremote`, `<exe dir>/lib/mediaremote`, and the
`Resources/mediaremote` or `lib/mediaremote` directory of the executable's
parent. The installer places the adapter in
`<version>/ultra-media-remote.app/Contents/Resources/mediaremote/`. It uses
the first directory containing both items; an invalid environment override
does not disable the other candidates. Missing or unusable adapter metadata
falls back to the direct path.

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
# Installer test against a locally built release dir:
./scripts/build-release.sh   # requires signing identity and NOTARY_PROFILE in env
ULTRA_MEDIA_REMOTE_TEST_RELEASE_DIR=dist/release cargo test --test installer
```

`src/lib.rs` contains Rust wrappers and mapping tests;
`native/UltraMediaRemote/` contains the Swift static library and analysis tests.
`build.rs` selects a separate Swift scratch directory for each profile/feature
variant; `ULTRA_MEDIA_REMOTE_SCRATCH` overrides its root. Cargo's job limit does
not propagate to the nested Swift build; resource-limited builders should
also constrain their Swift invocation.

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## License

MIT © 2026 Pareto Cybernetics. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Third-party notices

[MediaRemoteAdapter](https://github.com/ungive/mediaremote-adapter) is vendored under `third_party/mediaremote-adapter` and redistributed under its BSD 3-Clause license (© Jonas van den Berg and contributors); see [third_party/mediaremote-adapter/LICENSE](third_party/mediaremote-adapter/LICENSE). It is invoked at runtime through `/usr/bin/perl` and is not linked into binaries built against this crate.

The CoreAudio process-tap lifecycle was informed by
[AudioCap](https://github.com/insidegui/AudioCap) (© 2024 Guilherme Rambo);
its permissive license is reproduced at
[`third_party/AudioCap-LICENSE`](third_party/AudioCap-LICENSE).

---

Part of [Pareto Cybernetics](https://paretocybernetics.com/projects/ultra-media-remote/): free, private, open-source tools for working with AI agents.
