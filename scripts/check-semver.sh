#!/usr/bin/env bash
# Compare wiremux and wiremux-auth to the latest v0.11 tag.
# The pull request base is not the baseline: that commit can already
# contain the field under test.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

remote="${SEMVER_REMOTE:-origin}"
echo "PLAN: baseline is the latest v0.11 tag on ${remote}"
tag=$(git ls-remote --tags "$remote" 'refs/tags/v0.11.*' | python3 scripts/semver_baseline.py)
echo "DO: fetch ${tag}"
git fetch --no-tags "$remote" "refs/tags/${tag}:refs/tags/${tag}"
echo "DO: cargo semver-checks against ${tag}"
env -u RUSTFLAGS -u RUSTDOCFLAGS \
  cargo semver-checks check-release --baseline-rev "$tag" --package wiremux
env -u RUSTFLAGS -u RUSTDOCFLAGS \
  cargo semver-checks check-release --baseline-rev "$tag" --package wiremux-auth
echo "OK: wiremux and wiremux-auth match ${tag}"
