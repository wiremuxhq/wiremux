# Governance

One maintainer: Sebastien Tardif (`@SebTardif`).

## What lands

Changes land through a pull request. `main` requires one approving
review, a Developer Certificate of Origin sign-off (`git commit -s`),
and the required status checks. Squash is the only merge method.

`CONSTITUTION.md` outranks a normal pull request. Changes to that
file follow its Amending section. A person applies the `constitution`
label. That label is the yes to merge. Rejected proposals are listed
in [`docs/constitution-amendments.md`](docs/constitution-amendments.md).

## Releases

Release Please opens the version pull request. A maintainer merges it
when the notes are ready. That merge tags the release and publishes
`wiremux-auth` and `wiremux` to crates.io. Commits titled `docs:`,
`chore:`, or `ci:` do not bump the version.

## Where to talk

Bugs and feature requests: GitHub Issues.
Questions: GitHub Discussions.
Vulnerabilities: [`SECURITY.md`](SECURITY.md).
Day-to-day contribution steps: [`CONTRIBUTING.md`](CONTRIBUTING.md).
Support paths: [`SUPPORT.md`](SUPPORT.md).
