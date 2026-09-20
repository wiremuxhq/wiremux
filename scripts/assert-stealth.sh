#!/usr/bin/env bash
# Launch prep replaced stealth stubs. Tree check is assert-public.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec bash "$ROOT/scripts/assert-public.sh"
