#!/usr/bin/env bash
# Maps-only wiremux must not depend on clap, tokio, reqwest, or hyper.
# wiremux-auth may still use reqwest/tokio for TokenProvider.
set -euo pipefail
tree=$(cargo tree -p wiremux --no-default-features --edges normal --depth 1)
fail=0
for pkg in clap tokio reqwest hyper; do
  if printf '%s\n' "$tree" | grep -E "^${pkg} " >/dev/null; then
    printf 'wiremux --no-default-features must not depend on %s\n' "$pkg" >&2
    fail=1
  fi
done
if printf '%s\n' "$(cargo tree -p wiremux --no-default-features --edges normal --prefix none)" \
  | grep -E '^clap v' >/dev/null; then
  printf 'maps-only tree must not include clap at any depth\n' >&2
  fail=1
fi
if [[ "$fail" -ne 0 ]]; then
  printf '%s\n' "$tree" >&2
  exit 1
fi
echo "maps-only: wiremux has no clap, tokio, reqwest, or hyper"
