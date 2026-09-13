#!/usr/bin/env bash
# Refresh workspace package versions in Cargo.lock after release-please
# bumps crate Cargo.toml files. Does not re-resolve the whole graph.
set -euo pipefail

ROOT="${SYNC_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT"

echo "PLAN: refresh workspace versions in Cargo.lock"
if [[ ! -f Cargo.lock ]]; then
  echo "FAIL: Cargo.lock missing" >&2
  exit 1
fi

echo "DO: sync path-dep wiremux-auth version"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
python3 "$SCRIPT_DIR/sync-path-dep-versions.py" "$ROOT"

echo "DO: cargo check -p wiremux"
cargo check -p wiremux
echo "DO: cargo metadata --locked"
cargo metadata --locked --format-version 1 >/dev/null
echo "DONE: Cargo.lock matches crate versions"
