#!/usr/bin/env bash
# Maps-only wiremux must not pull HTTP/TLS/JWT at any depth.
# TokenProviders live on wiremux-auth feature `net`.
set -euo pipefail
tree=$(cargo tree -p wiremux --no-default-features --edges normal --prefix none)
fail=0
for pkg in clap tokio reqwest hyper jsonwebtoken aws-lc-sys; do
  if printf '%s\n' "$tree" | grep -E "^${pkg} v" >/dev/null; then
    printf 'wiremux --no-default-features must not depend on %s\n' "$pkg" >&2
    fail=1
  fi
done
if [[ "$fail" -ne 0 ]]; then
  printf '%s\n' "$tree" >&2
  exit 1
fi
echo "maps-only: wiremux has no clap, tokio, reqwest, hyper, jsonwebtoken, or aws-lc-sys"
