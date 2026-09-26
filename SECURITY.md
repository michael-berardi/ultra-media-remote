# Security policy

## Supported versions

Security fixes land on `main` and ship in the next release. Only the latest
release of ultra-media-remote is supported.

## Reporting a vulnerability

Please report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/michael-berardi/ultra-media-remote/security/advisories/new).
Do not open a public issue for an undisclosed vulnerability.

Include the affected version, your operating system, reproduction steps and
the impact you observed. Leave out credentials, personal data and private
files; a minimal synthetic reproduction is enough.

You can expect an acknowledgement within 7 days. Reporters are credited in
the release notes if they want to be.

## Scope

In scope: memory-safety issues in the FFI layer, unsafe handling of data
returned by the system media session, and anything that lets a caller reach
private framework functionality beyond the documented API. The vendored
`third_party/mediaremote-adapter` has its own upstream; issues specific to it
are forwarded there.
