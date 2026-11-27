<!-- update-contract: v1 -->
# Updates

The `ultra-media-remote` CLI tells you when a newer release exists and updates
itself with one command. Automatic installs stay off until you turn them on.

The library crate has none of this. It makes no network requests; an app that
links it owns its own updates.

## The daily check

After a command finishes on a terminal, the CLI may start a small detached
helper at low priority. At most once a day it makes one anonymous GET of the
public GitHub release record of the repository named in the package metadata:

```text
https://api.github.com/repos/michael-berardi/ultra-media-remote/releases/latest
```

The request carries the fixed User-Agent `ultra-media-remote-update-check`. It
sends no version, identifier, cookie or machine data, and curl config files are
ignored. It times out after 4 seconds. A failure is recorded with its cause and
is not retried until the next day. Only stable `x.y.z` releases newer than
yours count. Drafts, prereleases and other tags are ignored.

There is no check and no notice when stdout or stderr is not a terminal, when
`CI` is set, in agent shells (`CLAUDECODE`, `CODEX_SANDBOX`, `GEMINI_CLI`),
for `--version` and `--help`, with `ULTRA_MEDIA_REMOTE_NO_UPDATE_CHECK=1`, or
after `ultra-media-remote update --notify off`.

## The notice

The notice goes to stderr after your command, never into its JSON output:

```text
ultra-media-remote 0.4.0 available (you have 0.3.0) · run: ultra-media-remote update
turn on automatic updates: ultra-media-remote update --auto on · skip: ultra-media-remote update --skip
```

The second line is dim, and only offers `--skip` once automatic installs are
on. Each version is announced at most once a day. A skipped version stays
quiet until a newer one is released.

## Commands

| Command | Effect |
| --- | --- |
| `ultra-media-remote update` | Check now, show the new version and its notes, ask `Install 0.4.0 now? [Y/n]`, install |
| `ultra-media-remote update --check` | Report only: exit 0 up to date, 10 update available, 1 check failed (with the cause) |
| `ultra-media-remote update --skip` | Hide the current release's notice |
| `ultra-media-remote update --auto on\|off\|status` | Automatic installs (off by default) |
| `ultra-media-remote update --notify on\|off\|status` | The daily check and notice (on by default) |
| `ultra-media-remote update --rollback` | Switch back to the version that ran before the last update |

After a successful `update` on a terminal, the CLI asks once, `Install future
updates automatically? [y/N]`, and remembers the answer. Without a terminal,
`update` installs without asking, because running it is consent. Like every
command, `update` prints one JSON document on stdout.

With automatic installs on, the daily check installs a new release from the
detached helper. The new version runs from your next command. Each version is
tried once. If the install fails, your current version stays. The cause goes
into `update.log` and `update --auto status`, and the notice comes back.

## How updates are installed

`update` works on a copy from the one-line installer. It downloads the release
archive and `SHA256SUMS` from the GitHub release into a private folder inside
`~/.local/share/ultra-media-remote/`, and refuses the archive unless its
SHA-256 matches. It unpacks it, checks that the new binary starts and reports
the new version, moves it to `~/.local/share/ultra-media-remote/<version>/`
and switches the `current` link with one rename. The shim in `~/.local/bin`
runs `current`, so a command starting at that moment runs either the old
release or the new one. The release binary is Developer ID signed and
notarized, so Gatekeeper checks it too.

The release that was current stays for `update --rollback` until the new one
has completed a command (a week at most); then it is removed. Older release
folders are removed at once. The private download folder is deleted on success
and failure; one left by a killed update is removed by the next update.

A copy from `cargo install` or a local build is left alone: `update` prints the
installer command to run instead.

## Local files

| File | Contents |
| --- | --- |
| `update.json` (0600) | Last check time and result, skipped version, last notice, your two switches, the previous version |
| `update.log` (0600) | Automatic installs only, at most 256 KiB |

They live in `~/Library/Caches/ultra-media-remote/` (0700). For testing,
`ULTRA_MEDIA_REMOTE_UPDATE_CACHE` moves the folder,
`ULTRA_MEDIA_REMOTE_UPDATE_API` points the check at another release record and
`ULTRA_MEDIA_REMOTE_RELEASE_BASE` points downloads at another folder or URL.

`ultra-media-remote update --auto off` deletes both files and the folder. The
one exception is `--notify off`: a small file stays to remember that choice. To
uninstall completely:

```sh
ultra-media-remote update --notify on   # only if you turned notices off
ultra-media-remote update --auto off
rm -rf ~/.local/share/ultra-media-remote ~/.local/bin/ultra-media-remote
```
