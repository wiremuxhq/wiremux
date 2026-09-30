#!/usr/bin/env python3
"""Tests for scripts/package_release_binary.py."""

from __future__ import annotations

import hashlib
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

from package_release_binary import package_release_binary


class PackageReleaseBinaryTests(unittest.TestCase):
    def test_unix_archive_has_binary_at_root_and_sha256(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "wiremux"
            binary.write_bytes(b"not a real elf")
            license_path = root / "LICENSE"
            license_path.write_text("mit\n", encoding="utf-8")
            out = root / "dist"
            archive, sidecar = package_release_binary(
                binary,
                "x86_64-unknown-linux-gnu",
                out,
                [license_path, root / "missing"],
            )
            self.assertEqual(archive.name, "wiremux-x86_64-unknown-linux-gnu.tar.gz")
            with tarfile.open(archive, "r:gz") as tar:
                self.assertEqual(
                    sorted(tar.getnames()),
                    ["LICENSE", "wiremux"],
                )
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            self.assertTrue(sidecar.read_text(encoding="utf-8").startswith(digest + "  "))

    def test_windows_zip_uses_exe_name(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / "wiremux.exe"
            binary.write_bytes(b"MZ")
            archive, _sidecar = package_release_binary(
                binary,
                "x86_64-pc-windows-msvc",
                root / "dist",
                [],
            )
            self.assertEqual(archive.name, "wiremux-x86_64-pc-windows-msvc.zip")
            with zipfile.ZipFile(archive) as zipped:
                self.assertEqual(zipped.namelist(), ["wiremux.exe"])

    def test_missing_binary_exits(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(SystemExit):
                package_release_binary(
                    Path(tmp) / "nope",
                    "aarch64-apple-darwin",
                    Path(tmp) / "out",
                    [],
                )


if __name__ == "__main__":
    unittest.main()
