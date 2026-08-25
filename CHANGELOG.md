# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3] - 2026-08-25

### Added

- Capability-gated like and dislike state plus MediaRemote transport commands
  for players that explicitly advertise rating support.


## [0.2.0] - 2026-08-24

### Added

- Reusable `MediaSnapshot` feed combining Now Playing metadata, transport capabilities, and default-output volume.
- Public CoreAudio default-output volume read and normalized write controls.
- Non-prompting Screen Recording preflight, explicit user-action permission request, and hard start guard for the optional 11-band spectrum feed.
- Adapter-first absolute timeline seeking with direct runtime fallback.
- CoreAudio global-output spectrum startup for hosts migrating durable prior user consent.

### Fixed

- Direct transport fallback now runs when a staged adapter rejects delivery.
- Swift build products are isolated by Cargo feature set so default and spectrum builds cannot overwrite each other.

## [0.1.0] - 2026-08-24

### Added

- Runtime-only MediaRemote bridge using `dlopen` and `dlsym`, with graceful unavailable behavior on unsupported systems and non-macOS targets.
- Now Playing snapshots with title, artist, album, owning application identifiers, playback timing and state, plus bounded `image/` artwork data URLs.
- Media transport capability discovery and typed play/pause, previous, and next delivery.
- Poll-based subscription API with automatic unsubscription on drop.
- Adapter-first metadata fetch through the bundled BSD-3-Clause MediaRemoteAdapter, with direct MediaRemote fallback.
- `ULTRA_MEDIA_REMOTE_ADAPTER_DIR` override plus adjacent-binary and app-resource discovery for staged adapter files.
- Modern macOS 15.4+ metadata support when `MediaRemoteAdapter.framework` carries a valid Developer ID signature; ad-hoc-signed frameworks return empty sessions.
- Twelve Rust tests covering command mapping, capability resolution, metadata parsing, artwork bounds and MIME validation, adapter behavior, and subscriptions.
