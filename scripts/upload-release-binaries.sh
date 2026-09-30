#!/usr/bin/env bash
# Upload release archives, provenance, installers, and index files.
set -euo pipefail

echo "PLAN: upload release binaries"
: "${TAG:?TAG is required}"
: "${GH_REPO:?GH_REPO is required}"
: "${ASSET_DIR:?ASSET_DIR is required}"

if [ ! -d "$ASSET_DIR" ]; then
  echo "FAIL: asset dir missing: ${ASSET_DIR}" >&2
  exit 1
fi

found=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12; do
  if gh release view "$TAG" --repo "$GH_REPO" >/dev/null 2>&1; then
    found=1
    break
  fi
  echo "WAIT: release ${TAG} is not visible yet"
  sleep 5
done
if [ "$found" -ne 1 ]; then
  echo "FAIL: release ${TAG} was not created" >&2
  exit 1
fi

assets=()
while IFS= read -r path; do
  assets+=("$path")
done < <(find "$ASSET_DIR" -type f ! -name '.DS_Store' | sort)
if [ "${#assets[@]}" -eq 0 ]; then
  echo "FAIL: no wiremux archives in ${ASSET_DIR}" >&2
  exit 1
fi

names="$(for path in "${assets[@]}"; do basename "$path"; done | sort)"
dups="$(printf '%s\n' "$names" | uniq -d)"
if [ -n "$dups" ]; then
  echo "FAIL: duplicate asset names" >&2
  printf '%s\n' "$dups" >&2
  exit 1
fi

has_archive=0
for path in "${assets[@]}"; do
  case "$(basename "$path")" in
    wiremux-*.tar.gz|wiremux-*.zip) has_archive=1 ;;
  esac
done
if [ "$has_archive" -ne 1 ]; then
  echo "FAIL: no wiremux archive in ${ASSET_DIR}" >&2
  exit 1
fi

echo "DO: gh release upload ${TAG}"
gh release upload "$TAG" "${assets[@]}" --clobber --repo "$GH_REPO"
echo "DONE: uploaded ${#assets[@]} release binaries"
