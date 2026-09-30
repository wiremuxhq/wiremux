#!/usr/bin/env python3
"""Pack a wiremux release binary and write a SHA-256 sidecar.

Unix targets become tar.gz. Windows targets become zip. The executable
is at the archive root so a package index can point at one file.
"""

from __future__ import annotations

import argparse
import hashlib
import tarfile
import zipfile
from pathlib import Path


def archive_name(target: str) -> str:
    if "windows" in target:
        return f"wiremux-{target}.zip"
    return f"wiremux-{target}.tar.gz"


def executable_name(target: str) -> str:
    if "windows" in target:
        return "wiremux.exe"
    return "wiremux"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package_release_binary(
    binary: Path,
    target: str,
    out_dir: Path,
    licenses: list[Path],
) -> tuple[Path, Path]:
    if not binary.is_file():
        raise SystemExit(f"binary missing: {binary}")
    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / archive_name(target)
    exe_name = executable_name(target)
    members: list[tuple[str, Path]] = [(exe_name, binary)]
    for license_path in licenses:
        if license_path.is_file():
            members.append((license_path.name, license_path))
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as zipped:
            for arcname, src in members:
                zipped.write(src, arcname=arcname)
    else:
        with tarfile.open(archive, "w:gz") as tar:
            for arcname, src in members:
                tar.add(src, arcname=arcname)
    sidecar = Path(str(archive) + ".sha256")
    sidecar.write_text(f"{sha256_file(archive)}  {archive.name}\n", encoding="utf-8")
    return archive, sidecar


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--license", type=Path, action="append", default=[])
    args = parser.parse_args()
    archive, sidecar = package_release_binary(
        args.binary, args.target, args.out, args.license
    )
    print(f"OK: {archive}")
    print(f"OK: {sidecar}")


if __name__ == "__main__":
    main()
