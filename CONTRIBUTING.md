# Contributing

## Where to start

- [Good first issues](https://github.com/wiremuxhq/wiremux/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
- [Help wanted](https://github.com/wiremuxhq/wiremux/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)

Questions that are not bug reports go to [`SUPPORT.md`](SUPPORT.md).
Maintainer rules are in [`GOVERNANCE.md`](GOVERNANCE.md).

Open an issue before a large change. Small, tested fixes can go
straight to a pull request.

Changes to `CONSTITUTION.md` follow the Amending section in that
file. A person applies the `constitution` label. That label is the
yes to merge. Rejected proposals are listed in
[`docs/constitution-amendments.md`](docs/constitution-amendments.md).

## Local gate

The commands in `AGENTS.md` must pass on your workspace before you
open a pull request. In short:

```bash
make check
```

`make check` needs rustc 1.95 (see `rust-toolchain.toml`), rustfmt,
clippy, `cargo-deny`, actionlint 1.7.12, and zizmor 1.16.0.

Every commit needs a Developer Certificate of Origin trailer:

```bash
git commit -s
```

The sign-off email is `git config user.email`. The DCO workflow skips
bot commits and merge commits.

## Pull requests

Use the pull request template. Commits on `main` squash through the
required checks (Lint, Test, DCO, Stealth, CodeQL). The Stealth
check runs `scripts/assert-public.sh` (product README and crate
descriptions, not the old stub).

PR titles must be a conventional type (`feat`, `fix`, `docs`, `ci`,
`chore`, `test`, `refactor`, `perf`, `build`, `style`, `revert`).
The Semantic PR Title check enforces that. After squash-merge, that
title is what release-please reads:

| Title prefix | Next version |
| --- | --- |
| `feat` / `feat!` | minor (0.x while pre-1.0) |
| `fix` / `perf` | patch |
| `docs` / `chore` / `test` / `ci` / `refactor` | changelog only, no bump |

release-please opens a `chore(main): release X.Y.Z` PR and writes
`CHANGELOG.md`. That PR is labeled `autorelease: pending`. Do not
auto-merge it. Merging it creates the git tag, the GitHub Release,
and the crates.io publish job (OIDC trusted publishing, no long-lived
token).

Optional curated GitHub Release notes. Do not put them on `main`
and do not open a PR for them (that would start CI). A maintainer
may push a one-file branch named after the version:

```bash
# tag v0.2.2 -> branch release-note-0.2.2
git checkout --orphan release-note-0.2.2
git rm -rf --cached .
printf '%s\n' '# wiremux 0.2.2' > RELEASE_NOTES.md
git add RELEASE_NOTES.md
git commit -s -m "docs: notes for 0.2.2"
git push -u origin release-note-0.2.2
```

Do not merge the release-please pull request as part of this recipe.
A maintainer merges that pull request when the notes are ready.
That merge tags the release and publishes the crates. See
[GOVERNANCE.md](GOVERNANCE.md) Releases.

The Apply release notes workflow copies that file onto the Release
page and deletes the branch. No cleanup PR. A later
`gh workflow run "Apply release notes" -f tag=v0.2.2` does the same.

Or skip git: set Actions variables `RELEASE_NOTES` (markdown) and
`RELEASE_NOTES_TAG` (`v0.2.2` or `0.2.2`). The tag pin stops leftover
text applying to the next cut. Variables are not auto-deleted
(`GITHUB_TOKEN` cannot manage them).

## License

This project is dual-licensed under MIT or Apache-2.0. You may choose
either. See `LICENSE` (MIT) and `LICENSE-APACHE`.

## Conduct

See `CODE_OF_CONDUCT.md`. Security reports go to `SECURITY.md`, not
a public issue.
