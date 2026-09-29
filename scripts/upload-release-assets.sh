#!/usr/bin/env bash
# Upload each packaged crate and its *.intoto.jsonl provenance.
# Scorecard Signed-Releases reads release assets, not the Attestations API.
set -euo pipefail

echo "PLAN: upload crate and intoto provenance"
: "${TAG:?TAG is required}"
: "${GH_REPO:?GH_REPO is required}"
: "${AUTH_CRATE:?AUTH_CRATE is required}"
: "${WIREMUX_CRATE:?WIREMUX_CRATE is required}"
: "${AUTH_BUNDLE:?AUTH_BUNDLE is required}"
: "${WIREMUX_BUNDLE:?WIREMUX_BUNDLE is required}"

root="${GITHUB_WORKSPACE:-$(pwd)}"

resolve() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s\n' "${root}/$1" ;;
  esac
}

stage_intoto() {
  local crate="$1"
  local bundle="$2"
  local base dest
  if [ ! -f "$crate" ]; then
    echo "FAIL: crate missing: ${crate}" >&2
    exit 1
  fi
  if [ ! -f "$bundle" ]; then
    echo "FAIL: provenance bundle missing: ${bundle}" >&2
    exit 1
  fi
  base="$(basename "$crate")"
  dest="${RUNNER_TEMP:-/tmp}/${base}.intoto.jsonl"
  cp "$bundle" "$dest"
  printf '%s\n' "$dest"
}

auth_crate="$(resolve "$AUTH_CRATE")"
wiremux_crate="$(resolve "$WIREMUX_CRATE")"
auth_bundle="$(resolve "$AUTH_BUNDLE")"
wiremux_bundle="$(resolve "$WIREMUX_BUNDLE")"
auth_intoto="$(stage_intoto "$auth_crate" "$auth_bundle")"
wiremux_intoto="$(stage_intoto "$wiremux_crate" "$wiremux_bundle")"

echo "DO: gh release upload ${TAG}"
gh release upload "$TAG" \
  "$auth_crate" \
  "$auth_intoto" \
  "$wiremux_crate" \
  "$wiremux_intoto" \
  --clobber \
  --repo "$GH_REPO"
echo "DONE: uploaded crates and intoto provenance"
