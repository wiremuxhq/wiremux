#!/usr/bin/env python3
"""Pin wiremux's path-dep version to the wiremux-auth package version."""

from __future__ import annotations

import re
import sys
from pathlib import Path


def sync_path_dep(root: Path) -> str:
    auth = (root / "crates/wiremux-auth/Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?m)^version = "([^"]+)"', auth)
    if match is None:
        raise SystemExit("FAIL: crates/wiremux-auth/Cargo.toml has no package version")
    version = match.group(1)
    path = root / "crates/wiremux/Cargo.toml"
    old = path.read_text(encoding="utf-8")
    new, n = re.subn(
        r'wiremux-auth = \{ version = "[^"]+", path = "../wiremux-auth" \}',
        f'wiremux-auth = {{ version = "{version}", path = "../wiremux-auth" }}',
        old,
        count=1,
    )
    if n != 1:
        raise SystemExit("FAIL: crates/wiremux/Cargo.toml path-dep pin not found")
    if new != old:
        path.write_text(new, encoding="utf-8")
        return f"DO: path-dep wiremux-auth -> {version}"
    return f"OK: path-dep already {version}"


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    print(sync_path_dep(root))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
