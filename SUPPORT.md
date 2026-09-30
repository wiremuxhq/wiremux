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
macOS Apple silicon, and Windows x64. Each archive has the `wiremux`
binary at the root and a `.sha256` sidecar.

Homebrew, winget, Scoop, and Chocolatey packages are not published.
Those indexes still need a release binary, which the archives are.
They are not formulas or manifests.
