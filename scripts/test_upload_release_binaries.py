#!/usr/bin/env python3
"""Tests for scripts/upload-release-binaries.sh."""

from __future__ import annotations

import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
UPLOAD = ROOT / "scripts" / "upload-release-binaries.sh"

GH_STUB = """#!/bin/sh
printf '%s\\n' CALL "$@" >> "$GH_STUB_LOG"
if [ "${1:-}" = "release" ] && [ "${2:-}" = "view" ]; then
  if [ "${GH_STUB_VIEW_FAIL:-}" = "1" ]; then
    exit 1
  fi
  exit 0
fi
exit 99
"""


def gh_calls(log: str) -> list[list[str]]:
    groups: list[list[str]] = []
    current: list[str] | None = None
    for line in log.splitlines():
        if line == "CALL":
            current = []
            groups.append(current)
            continue
        if current is None:
            continue
        current.append(line)
    return groups


def uploaded(log: str) -> bool:
    return any(call[:2] == ["release", "upload"] for call in gh_calls(log))


class UploadReleaseBinariesTests(unittest.TestCase):
    def _run(
        self,
        asset_dir: Path | None,
        *,
        extra: dict[str, str] | None = None,
        timeout: float = 5,
    ) -> tuple[subprocess.CompletedProcess[str], str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            log = root / "gh.log"
            log.write_text("", encoding="utf-8")
            stub = bin_dir / "gh"
            stub.write_text(GH_STUB, encoding="utf-8")
            stub.chmod(0o755)
            if asset_dir is None:
                asset_value = str(root / "missing-assets")
            else:
                asset_value = str(asset_dir)
            env = {
                "PATH": os.pathsep.join([str(bin_dir), "/bin", "/usr/bin"]),
                "TAG": "v0.9.3",
                "GH_REPO": "wiremuxhq/wiremux",
                "ASSET_DIR": asset_value,
                "GH_STUB_LOG": str(log),
                "HOME": str(root),
            }
            if extra:
                env.update(extra)
            result = subprocess.run(
                ["bash", str(UPLOAD)],
                check=False,
                capture_output=True,
                text=True,
                env=env,
                timeout=timeout,
            )
            return result, log.read_text(encoding="utf-8")

    def test_missing_asset_dir_fails_before_gh(self) -> None:
        result, log = self._run(None)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("asset dir missing", result.stderr)
        self.assertEqual(log, "")

    def test_empty_dir_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result, log = self._run(Path(tmp))
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("no wiremux archives", result.stderr)
        self.assertFalse(uploaded(log))

    def test_duplicate_asset_names_fail_before_upload(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a").mkdir()
            (root / "b").mkdir()
            (root / "a" / "wiremux.rb").write_text("one\n", encoding="utf-8")
            (root / "b" / "wiremux.rb").write_text("two\n", encoding="utf-8")
            (root / "wiremux-x86_64-unknown-linux-gnu.tar.gz").write_bytes(b"tar")
            result, log = self._run(root)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("duplicate asset names", result.stderr)
        self.assertFalse(uploaded(log))

    def test_sidecar_only_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            name = "wiremux-x86_64-unknown-linux-gnu.tar.gz.sha256"
            (root / name).write_text("abc\n", encoding="utf-8")
            result, log = self._run(root)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("no wiremux archive", result.stderr)
        self.assertNotIn("no wiremux archives", result.stderr)
        self.assertFalse(uploaded(log))

    def test_release_never_visible_fails_fast(self) -> None:
        text = UPLOAD.read_text(encoding="utf-8")
        self.assertIn("${UPLOAD_POLL_ATTEMPTS:-12}", text)
        self.assertIn("${UPLOAD_POLL_SLEEP:-5}", text)
        self.assertNotIn('TAG="${TAG}"', text)
        with tempfile.TemporaryDirectory() as tmp:
            started = time.perf_counter()
            result, log = self._run(
                Path(tmp),
                extra={
                    "GH_STUB_VIEW_FAIL": "1",
                    "UPLOAD_POLL_ATTEMPTS": "2",
                    "UPLOAD_POLL_SLEEP": "0",
                },
            )
            elapsed = time.perf_counter() - started
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("was not created", result.stderr)
        self.assertLess(elapsed, 2.0, result.stderr)
        self.assertEqual(
            [call[:2] for call in gh_calls(log)],
            [["release", "view"], ["release", "view"]],
        )
        self.assertFalse(uploaded(log))


if __name__ == "__main__":
    unittest.main()
