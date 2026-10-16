#!/usr/bin/env python3
"""Pick the latest v0.11 patch tag. Stdin is `git ls-remote --tags`."""

from __future__ import annotations

import sys


def latest_v011(lines: list[str]) -> str | None:
    """Highest `v0.11.<digits>` tag. Annotated peels (`^{}`) do not count twice."""
    best_n = -1
    best: str | None = None
    for line in lines:
        parts = line.split()
        if len(parts) < 2:
            continue
        ref = parts[-1]
        if ref.endswith("^{}"):
            ref = ref[:-3]
        name = ref.rsplit("/", 1)[-1]
        prefix = "v0.11."
        if not name.startswith(prefix):
            continue
        patch = name[len(prefix) :]
        if not patch.isdigit():
            continue
        number = int(patch)
        if number > best_n:
            best_n = number
            best = name
    return best


def main() -> int:
    chosen = latest_v011(sys.stdin.read().splitlines())
    if chosen is None:
        print("FAIL: no v0.11.* tag", file=sys.stderr)
        return 1
    print(chosen)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
