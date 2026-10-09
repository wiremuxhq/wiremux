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

# Sparse index layout: https://doc.rust-lang.org/cargo/reference/registry-index.html
index_rel() {
  local name="$1"
  local n=${#name}
  if [ "$n" -eq 1 ]; then
    printf '1/%s' "$name"
  elif [ "$n" -eq 2 ]; then
    printf '2/%s' "$name"
  elif [ "$n" -eq 3 ]; then
    printf '3/%s/%s' "${name:0:1}" "$name"
  else
    printf '%s/%s/%s' "${name:0:2}" "${name:2:2}" "$name"
  fi
}

# cargo publish reads this index. The crates.io API can show a version first.
wait_for_index() {
  local crate="$1"
  local version="$2"
  local deadline="${INDEX_WAIT_SECS:-300}"
  local pause="${INDEX_POLL_SECS:-10}"
  local rel url start now elapsed remaining body
  rel="$(index_rel "$crate")"
  url="https://index.crates.io/${rel}"
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    elapsed=$((now - start))
    remaining=$((deadline - elapsed))
    if [ "$remaining" -le 0 ]; then
      echo "FAIL: crates.io index did not list ${crate} ${version} within ${deadline}s" >&2
      return 1
    fi
    body="$("${CURL}" -fsS -A "${USER_AGENT}" "$url" 2>/dev/null || true)"
    if printf '%s\n' "$body" | grep -F "\"vers\":\"${version}\"" >/dev/null; then
      echo "OK: index lists ${crate} ${version}"
      return 0
    fi
    echo "WAIT: index does not list ${crate} ${version} yet (${remaining}s left)"
    if [ "$pause" -gt "$remaining" ]; then
      sleep "$remaining"
    else
      sleep "$pause"
    fi
  done
}

if [ "${PUBLISH_CRATES_CMD:-}" = "wait-index" ]; then
  wait_for_index "${1:?crate}" "${2:?version}"
  exit 0
fi

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

pending_crate=""
pending_version=""
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

  # The next crate cannot select a dependency that the index does not list yet.
  if [ -n "${pending_crate}" ]; then
    wait_for_index "${pending_crate}" "${pending_version}"
  fi

  echo "DO: ${CARGO} publish --locked -p ${crate}"
  set +e
  "${CARGO}" publish --locked -p "${crate}" >"${tmp}" 2>&1
  st=$?
  set -e
  cat "${tmp}"

  if [ "${st}" -eq 0 ]; then
    echo "DONE: published ${crate} ${version}"
    pending_crate="${crate}"
    pending_version="${version}"
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
