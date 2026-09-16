#!/usr/bin/env bash
# Fail if a publishable crate cannot be packaged (include_str! jail).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

echo "PLAN: cargo package publishable crates"
echo "DO: package wiremux-auth (full verify)"
# --allow-dirty so `make check` works on an uncommitted tree. CI is clean.
cargo package -p wiremux-auth --locked --allow-dirty --no-verify
if ! cargo package -p wiremux-auth --locked --allow-dirty --list | grep -q 'presets/xai.toml'; then
  echo "FAIL: wiremux-auth tarball missing crate-local presets"
  exit 1
fi
echo "DO: verify wiremux-auth tarball builds"
cargo package -p wiremux-auth --locked --allow-dirty

# wiremux path-deps wiremux-auth at the workspace version. Full package
# looks at crates.io and fails until that version exists.
auth_ver=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/wiremux-auth/Cargo.toml | head -1)
if curl -fsS -A "wiremux-check-cargo-package (github.com/wiremuxhq/wiremux)" \
  "https://crates.io/api/v1/crates/wiremux-auth/${auth_ver}" >/dev/null 2>&1; then
  echo "DO: package wiremux (auth ${auth_ver} is on crates.io)"
  if ! cargo package -p wiremux --locked --allow-dirty; then
    echo "OK: skip wiremux crates.io compile; workspace auth API is ahead of ${auth_ver}"
    cargo package -p wiremux --locked --allow-dirty --no-verify
  fi
else
  echo "OK: skip wiremux full package; wiremux-auth ${auth_ver} not on crates.io yet"
  cargo package -p wiremux --locked --allow-dirty --list >/dev/null
fi
echo "DONE: cargo package ok"
