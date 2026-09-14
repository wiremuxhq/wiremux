#!/usr/bin/env bash
# Publish wiremux-auth, then wiremux, when this tree's versions are
# not on crates.io yet. Skip (exit 0) a crate if crates.io already
# has that version or cargo says already uploaded. Missing source or
# a real publish error is exit 1.
set -euo pipefail

TAG="${TAG:-}"
DRY_RUN="${DRY_RUN:-0}"
CARGO="${CARGO:-cargo}"
CURL="${CURL:-curl}"
USER_AGENT="${USER_AGENT:-wiremux-publish (https://github.com/wiremuxhq/wiremux)}"
CRATES=(wiremux-auth wiremux)

echo "PLAN: publish ${CRATES[*]} from $(pwd)"

if [ ! -f Cargo.toml ] || [ ! -d crates/wiremux-auth ] || [ ! -d crates/wiremux ]; then
  echo "FAIL: workspace root with crates/wiremux-auth and crates/wiremux required" >&2
  exit 1
fi

crate_version() {
  local crate="$1"
  python3 - "$crate" <<'PY'
import sys
import tomllib
from pathlib import Path

crate = sys.argv[1]
path = Path("crates") / crate / "Cargo.toml"
print(tomllib.loads(path.read_text(encoding="utf-8"))["package"]["version"])
PY
}

published=0
for crate in "${CRATES[@]}"; do
  version="$(crate_version "${crate}")"
  if [ -z "${version}" ]; then
    echo "FAIL: no package.version in crates/${crate}/Cargo.toml" >&2
    exit 1
  fi
  if [ -n "${TAG}" ]; then
    semver="${TAG#v}"
    if [ "${semver}" != "${version}" ]; then
      echo "FAIL: tag ${TAG} does not match ${crate} ${version}" >&2
      exit 1
    fi
  fi

  echo "PLAN: ${crate} ${version}"
  tmp="$(mktemp)"
  http="$("${CURL}" -sS -o "${tmp}" -w '%{http_code}' -A "${USER_AGENT}" \
    "https://crates.io/api/v1/crates/${crate}/${version}" || true)"

  if [ "${http}" = "200" ]; then
    echo "OK: ${crate} ${version} already on crates.io"
    rm -f "${tmp}"
    continue
  fi

  if [ "${http}" != "404" ]; then
    echo "FAIL: crates.io GET ${crate}/${version} HTTP ${http}" >&2
    if [ -s "${tmp}" ]; then
      head -c 400 "${tmp}" >&2
      echo >&2
    fi
    rm -f "${tmp}"
    exit 1
  fi

  if [ "${DRY_RUN}" = "1" ]; then
    echo "DRY_RUN: would cargo publish --locked -p ${crate} ${version}"
    rm -f "${tmp}"
    continue
  fi

  if [ -z "${CARGO_REGISTRY_TOKEN:-}" ]; then
    echo "FAIL: CARGO_REGISTRY_TOKEN is unset" >&2
    rm -f "${tmp}"
    exit 1
  fi

  if [ "${published}" -gt 0 ]; then
    echo "WAIT: 60s before the next new upload"
    sleep 60
  fi

  echo "DO: ${CARGO} publish --locked -p ${crate}"
  set +e
  "${CARGO}" publish --locked -p "${crate}" >"${tmp}" 2>&1
  st=$?
  set -e
  cat "${tmp}"

  if [ "${st}" -eq 0 ]; then
    echo "DONE: published ${crate} ${version}"
    published=$((published + 1))
    rm -f "${tmp}"
    continue
  fi

  if grep -qiE 'already uploaded|already exists' "${tmp}"; then
    echo "OK: cargo reported already uploaded for ${crate}"
    rm -f "${tmp}"
    continue
  fi

  echo "FAIL: cargo publish -p ${crate} exited ${st}" >&2
  rm -f "${tmp}"
  exit "${st}"
done

echo "DONE: publish pass finished"
