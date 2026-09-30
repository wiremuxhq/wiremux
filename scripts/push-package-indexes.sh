#!/usr/bin/env bash
# Push the Homebrew formula and Scoop manifest when TOKEN is set.
# TOKEN is HOMEBREW_TAP_TOKEN. An empty token skips both repos.
set -euo pipefail

echo "PLAN: push Homebrew formula and Scoop manifest"
: "${TAG:?TAG is required}"
: "${GH_REPO:?GH_REPO is required}"
: "${ASSET_DIR:?ASSET_DIR is required}"

if [ -z "${TOKEN:-}" ]; then
  echo "OK: HOMEBREW_TAP_TOKEN unset; skip Homebrew tap and Scoop bucket"
  exit 0
fi

version="${TAG#v}"
formula="${ASSET_DIR}/Formula/wiremux.rb"
scoop="${ASSET_DIR}/bucket/wiremux.json"
if [ ! -f "$formula" ]; then
  formula="${ASSET_DIR}/wiremux.rb"
fi
if [ ! -f "$scoop" ]; then
  scoop="${ASSET_DIR}/wiremux.json"
fi
if [ ! -f "$formula" ] || [ ! -f "$scoop" ]; then
  echo "FAIL: formula or scoop manifest missing in ${ASSET_DIR}" >&2
  exit 1
fi

export GH_TOKEN="$TOKEN"
gh auth setup-git
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

push_repo() {
  local repo="$1"
  local src="$2"
  local dest_rel="$3"
  local message="$4"
  local dir="$work/${repo##*/}"
  echo "DO: clone ${repo}"
  if ! gh repo clone "$repo" "$dir"; then
    echo "FAIL: cannot clone ${repo}" >&2
    exit 1
  fi
  mkdir -p "$dir/$(dirname "$dest_rel")"
  cp "$src" "$dir/$dest_rel"
  git -C "$dir" config user.name "github-actions[bot]"
  git -C "$dir" config user.email "41898282+github-actions[bot]@users.noreply.github.com"
  git -C "$dir" add "$dest_rel"
  if git -C "$dir" diff --cached --quiet; then
    echo "OK: ${repo} already current"
    return 0
  fi
  git -C "$dir" commit -s -m "$message"
  git -C "$dir" -c "http.extraheader=AUTHORIZATION: bearer ${TOKEN}" push origin HEAD:main
  echo "OK: pushed ${repo}"
}

push_repo "wiremuxhq/homebrew-tap" "$formula" "Formula/wiremux.rb" "wiremux ${version}"
push_repo "wiremuxhq/scoop-bucket" "$scoop" "bucket/wiremux.json" "wiremux ${version}"
echo "DONE: package indexes"
