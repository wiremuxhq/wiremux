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

Homebrew:

```bash
brew tap wiremuxhq/tap
brew trust wiremuxhq/tap
brew install wiremux
```

Homebrew 6 and later refuse an untrusted tap until `brew trust`.
The formula lives in
[wiremuxhq/homebrew-tap](https://github.com/wiremuxhq/homebrew-tap).

Scoop:

```bash
scoop bucket add wiremux https://github.com/wiremuxhq/scoop-bucket
scoop install wiremux
```

`scoop search` does not look in that bucket until it is added.
The manifest is in
[wiremuxhq/scoop-bucket](https://github.com/wiremuxhq/scoop-bucket).

Later releases push those two repositories only when
`HOMEBREW_TAP_TOKEN` is set on this repo. That secret is not set, so
a release does not update the tap or the bucket by itself.

winget manifests are generated on the release. The first package pull
request is
[microsoft/winget-pkgs#444315](https://github.com/microsoft/winget-pkgs/pull/444315).
`winget install` does not work until that pull request merges.
Later releases open an update only when `WINGET_TOKEN` is set, and
only after this first package is in the community repository.

Chocolatey files are on the release. The package is not on the
community feed. Publishing needs `CHOCOLATEY_API_KEY`, which is not
set.

Git tags are not GPG-signed. The package signature is the
`.intoto.jsonl` provenance file next to each archive.
