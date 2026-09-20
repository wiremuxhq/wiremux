#!/usr/bin/env bash
# Fail if stealth-public stubs remain on launch surfaces in the tree.
# Repo About and topics are launch-day GitHub settings, not this check.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

fail() { echo "public surface: $*" >&2; exit 1; }

readme="$(tr -d '\r' < "$ROOT/README.md")"
printf '%s\n' "$readme" | grep -qx '# Wiremux' || fail "README title must be # Wiremux"
if printf '%s\n' "$readme" | grep -qx 'Not ready.'; then
  fail "README still says Not ready."
fi
printf '%s\n' "$readme" | grep -q 'TokenProvider' || fail "README must mention TokenProvider"

if grep -E 'description = "Reserved\."' "$ROOT"/crates/*/Cargo.toml; then
  fail "crate description still Reserved."
fi
if grep -E 'keywords = \[\]' "$ROOT"/crates/*/Cargo.toml; then
  fail "crate keywords empty"
fi
if grep -F 'about = "Reserved."' "$ROOT"/crates/wiremux/src/bin/wiremux.rs; then
  fail "CLI about still Reserved."
fi
if head -1 "$ROOT"/crates/wiremux/src/lib.rs | grep -q 'Not ready.'; then
  fail "wiremux rustdoc still says Not ready."
fi
if head -1 "$ROOT"/crates/wiremux-auth/src/lib.rs | grep -q 'Not ready.'; then
  fail "wiremux-auth rustdoc still says Not ready."
fi
png="$ROOT/docs/brand/social-preview.png"
test -s "$png" || fail "social-preview.png missing"
if grep -F 'Stealth-public until' "$ROOT/CONSTITUTION.md"; then
  fail "CONSTITUTION still stealth-public"
fi

preview="$ROOT/docs/brand/social-preview.svg"
grep -q 'Wiremux' "$preview" || fail "social preview missing Wiremux wordmark"
grep -q 'TokenProvider' "$preview" || fail "social preview missing tagline"

echo "public surfaces ok"
