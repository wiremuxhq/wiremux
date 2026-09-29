#!/usr/bin/env bash
# Package wiremux-auth and wiremux for the GitHub Release.
# Reuse target/package/*.crate when cargo publish already wrote it.
set -euo pipefail

echo "PLAN: package wiremux crates for the GitHub release"
if [ -z "${GITHUB_OUTPUT:-}" ]; then
  echo "FAIL: GITHUB_OUTPUT is unset" >&2
  exit 1
fi
if [ ! -f Cargo.toml ] || [ ! -d crates/wiremux-auth ] || [ ! -d crates/wiremux ]; then
  echo "FAIL: run this from the crate workspace root" >&2
  exit 1
fi

root=$(pwd)

crate_version() {
  python3 - "$1" <<'PY'
import sys
import tomllib
from pathlib import Path

crate = sys.argv[1]
path = Path("crates") / crate / "Cargo.toml"
print(tomllib.loads(path.read_text(encoding="utf-8"))["package"]["version"])
PY
}

package_one() {
  local name="$1"
  local key="$2"
  local version crate_path
  version="$(crate_version "$name")"
  crate_path="${root}/target/package/${name}-${version}.crate"
  if [ ! -f "$crate_path" ]; then
    echo "DO: cargo package --locked -p ${name}"
    cargo package --locked -p "$name"
  else
    echo "OK: reuse ${crate_path}"
  fi
  if [ ! -f "$crate_path" ]; then
    echo "FAIL: missing ${crate_path}" >&2
    if [ -d target/package ]; then
      ls -la target/package >&2
    fi
    exit 1
  fi
  echo "${key}=${crate_path}" >> "$GITHUB_OUTPUT"
}

package_one wiremux-auth auth_crate
package_one wiremux wiremux_crate
echo "DONE: packaged crates"
