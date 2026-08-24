# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
