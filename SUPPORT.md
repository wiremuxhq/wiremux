# Support

## Bugs and features

Open an issue:

https://github.com/wiremuxhq/wiremux/issues/new/choose

Good first issues and help wanted are linked from
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Questions

https://github.com/wiremuxhq/wiremux/discussions

## Security

Do not file a public issue for a vulnerability. Use the private
report form in [`SECURITY.md`](SECURITY.md).

## Install

The supported install is Cargo. See the Install section in
[`README.md`](README.md). `cargo install wiremux --locked` installs
the CLI.

A `vX.Y.Z` GitHub Release also gets portable archives from
[Release binaries](.github/workflows/release-binaries.yml): Linux x64,
Linux arm64, macOS Apple silicon, macOS Intel, and Windows x64. Each
archive has the `wiremux` binary at the root, a `.sha256` sidecar, and
a `.intoto.jsonl` build provenance file. The same release includes
`wiremux-sbom.cdx.json`, `wiremux-installer.sh`, and
`wiremux-installer.ps1`. Those installers are pinned to that tag and
check the archive SHA-256 before copying the binary.

Homebrew (`wiremuxhq/homebrew-tap`) and Scoop (`wiremuxhq/scoop-bucket`)
files are generated on each release. The workflow pushes them when
`HOMEBREW_TAP_TOKEN` is set. Until those repositories contain the
current version, `brew` and `scoop` are not install paths.

winget and Chocolatey files are on the release too. winget opens a
pull request when `WINGET_TOKEN` is set, after the first package is
already in the community repository. Chocolatey pushes when
`CHOCOLATEY_API_KEY` is set. Neither one is an install path until the
upstream review accepts the current version.

Git tags are not GPG-signed. The package signature is the
`.intoto.jsonl` provenance file next to each archive.
