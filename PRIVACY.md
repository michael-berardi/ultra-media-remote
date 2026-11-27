# Privacy

ultra-media-remote has no telemetry, analytics, crash reporting, remote
configuration or install ID. Track metadata and audio levels stay on your Mac.

## Network

**The library crate** sends nothing over the network. It makes no outbound
HTTP, DNS or socket connections and opens no listener. No web UI or login
surface ships with it. If your app sends track metadata elsewhere, that is your
app's behavior, not this crate's.

**The CLI** makes one request on its own: the daily update check.

- One anonymous GET of this repository's public GitHub release record,
  `https://api.github.com/repos/michael-berardi/ultra-media-remote/releases/latest`.
- Fixed User-Agent `ultra-media-remote-update-check`. No version, identifier,
  cookie, token or machine data, and curl config files are ignored. As with
  any web request, GitHub sees your IP address.
- At most once a day, from a detached helper, with a 4-second timeout.
- Never in CI, in agent shells, for `--version`/`--help` or without a
  terminal. To turn it off, set `ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK=1` or run
  `ultra-media-remote update --notify off`.

`ultra-media-remote update`, and automatic installs if you turn them on,
download the release archive and `SHA256SUMS` from the GitHub release with the
same fixed User-Agent. The one-line installer downloads the same files when you
run it. `cargo add --git` fetches source from GitHub because you choose to run
it. See [docs/updates.md](docs/updates.md).

## Local processing

The crate reads Now Playing metadata and controls playback through local macOS
APIs (MediaRemote and CoreAudio). If you stage the optional MediaRemoteAdapter,
the crate runs it as a local Perl subprocess; a staged adapter is code you
supply yourself. The optional `spectrum` feature reads system audio only after
your app has Screen Recording permission.

## Local files

- **The library stores nothing on disk**: no logs, history, cache files or
  credentials. Metadata and audio levels stay in memory; adapter results are
  cached in memory for two seconds. The examples print metadata to your
  terminal when you run them.
- Building from source creates `target/` and
  `native/UltraMediaRemote/.build-cargo/` in the checkout. To remove them:
  `rm -rf target native/UltraMediaRemote/.build-cargo`.
- The installer copy lives in `~/.local/share/ultra-media-remote/` (0700 when
  the installer creates it): the release in use and at most one earlier
  release for `update --rollback`, plus the shim
  `~/.local/bin/ultra-media-remote`.
- The CLI's update state is `update.json` (0600) in
  `~/Library/Caches/ultra-media-remote/` (0700). Automatic installs also write
  `update.log` there, capped at 256 KiB. `ultra-media-remote update --auto off`
  deletes both files.

To remove everything the CLI wrote:

```sh
ultra-media-remote update --notify on   # only if you turned notices off
ultra-media-remote update --auto off
rm -rf ~/.local/share/ultra-media-remote ~/.local/bin/ultra-media-remote
```

## Third parties

No third-party service is contacted. GitHub serves the release record and
release files. The README carries no remote images.
