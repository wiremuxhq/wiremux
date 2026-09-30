#!/usr/bin/env python3
"""Tests for scripts/render_release_extras.py."""

from __future__ import annotations

import hashlib
import os
import stat
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

from render_release_extras import DESCRIPTION, HOMEBREW_DESC, render


ROOT = Path(__file__).resolve().parents[1]


def digest(label: str) -> str:
    return hashlib.sha256(label.encode("utf-8")).hexdigest()


def write_sidecars(root: Path, targets: dict[str, str], nested: bool) -> None:
    for target, value in targets.items():
        if "windows" in target:
            name = f"wiremux-{target}.zip.sha256"
        else:
            name = f"wiremux-{target}.tar.gz.sha256"
        directory = root / "nested" / target if nested else root
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_text(f"{value}  {name[:-len('.sha256')]}\n", encoding="utf-8")


class RenderReleaseExtrasTests(unittest.TestCase):
    def setUp(self) -> None:
        self.targets = {
            "x86_64-unknown-linux-gnu": digest("linux"),
            "aarch64-unknown-linux-gnu": digest("linux-arm"),
            "aarch64-apple-darwin": digest("mac-arm"),
            "x86_64-apple-darwin": digest("mac-intel"),
            "x86_64-pc-windows-msvc": digest("windows"),
        }

    def render_targets(self, targets: dict[str, str], nested: bool = False) -> dict[str, bytes]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_sidecars(root, targets, nested)
            return render("v0.9.3", root, "wiremuxhq/wiremux")

    def test_description_matches_crate(self) -> None:
        text = (ROOT / "crates/wiremux/Cargo.toml").read_text(encoding="utf-8")
        self.assertIn(f'description = "{DESCRIPTION}"', text)

    def test_full_set_is_stable_and_complete(self) -> None:
        first = self.render_targets(self.targets, nested=True)
        second = self.render_targets(self.targets, nested=True)
        self.assertEqual(first, second)
        formula = first["Formula/wiremux.rb"].decode("utf-8")
        self.assertIn("class Wiremux < Formula", formula)
        self.assertIn(f'desc "{HOMEBREW_DESC}"', formula)
        self.assertLess(len(HOMEBREW_DESC), 80)
        self.assertFalse(HOMEBREW_DESC.startswith("Wiremux"))
        self.assertFalse(HOMEBREW_DESC.endswith("."))
        self.assertNotIn('version "', formula)
        self.assertIn('license any_of: ["MIT", "Apache-2.0"]', formula)
        self.assertIn("/v0.9.3/", formula)
        self.assertIn("on_macos do", formula)
        self.assertIn("on_intel do", formula)
        self.assertIn("on_arm do", formula)
        self.assertIn("on_linux do", formula)
        self.assertIn(self.targets["x86_64-apple-darwin"], formula)
        self.assertIn(self.targets["aarch64-unknown-linux-gnu"], formula)
        self.assertIn("wiremux-x86_64-apple-darwin.tar.gz", formula)
        scoop = first["bucket/wiremux.json"].decode("utf-8")
        self.assertIn("\n    ", scoop)
        self.assertIn('"license": "MIT|Apache-2.0"', scoop)
        self.assertNotIn("arm64", scoop)
        self.assertIn(self.targets["x86_64-pc-windows-msvc"], scoop)
        installer = first["winget/Wiremux.Wiremux.installer.yaml"].decode("utf-8")
        self.assertIn("RelativeFilePath: wiremux.exe", installer)
        self.assertIn(self.targets["x86_64-pc-windows-msvc"].upper(), installer)
        self.assertIn("Microsoft.VCRedist.2015+.x64", installer)
        self.assertIn("PackageIdentifier: Wiremux.Wiremux", installer)
        shell = first["wiremux-installer.sh"].decode("utf-8")
        self.assertIn(self.targets["aarch64-apple-darwin"], shell)
        self.assertIn('version="0.9.3"', shell)
        self.assertNotIn("v0.9.3", shell.split("download/v", 1)[0])
        powershell = first["wiremux-installer.ps1"].decode("utf-8")
        self.assertIn(self.targets["x86_64-pc-windows-msvc"], powershell)
        ps1 = first["chocolatey/tools/chocolateyInstall.ps1"]
        self.assertTrue(ps1.startswith(b"\xef\xbb\xbf"))
        self.assertIn(self.targets["x86_64-pc-windows-msvc"].encode("utf-8"), ps1)
        root = ET.fromstring(first["chocolatey/wiremux.nuspec"])
        ns = {"n": "http://schemas.microsoft.com/packaging/2015/06/nuspec.xsd"}
        self.assertEqual(root.findtext("n:metadata/n:version", namespaces=ns), "0.9.3")
        self.assertNotEqual(
            root.findtext("n:metadata/n:projectUrl", namespaces=ns),
            root.findtext("n:metadata/n:projectSourceUrl", namespaces=ns),
        )

    def test_missing_optional_arch_omits_that_block(self) -> None:
        targets = dict(self.targets)
        del targets["x86_64-apple-darwin"]
        del targets["aarch64-unknown-linux-gnu"]
        files = self.render_targets(targets)
        formula = files["Formula/wiremux.rb"].decode("utf-8")
        self.assertNotIn("x86_64-apple-darwin", formula)
        self.assertNotIn("aarch64-unknown-linux-gnu", formula)
        self.assertIn("aarch64-apple-darwin", formula)
        self.assertIn("x86_64-unknown-linux-gnu", formula)
        shell = files["wiremux-installer.sh"].decode("utf-8")
        self.assertNotIn("x86_64-apple-darwin)", shell)

    def test_missing_required_hash_fails(self) -> None:
        targets = dict(self.targets)
        del targets["x86_64-pc-windows-msvc"]
        with self.assertRaises(SystemExit) as caught:
            self.render_targets(targets)
        self.assertIn("x86_64-pc-windows-msvc", str(caught.exception))

    def test_bad_sidecar_fails(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            write_sidecars(root, self.targets, nested=False)
            bad = root / "wiremux-x86_64-pc-windows-msvc.zip.sha256"
            bad.write_text("not-a-hash\n", encoding="utf-8")
            with self.assertRaises(SystemExit):
                render("0.9.3", root, "wiremuxhq/wiremux")

    def test_check_and_shell_syntax(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            assets = root / "assets"
            out = root / "out"
            write_sidecars(assets, self.targets, nested=True)
            subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/render_release_extras.py"),
                    "--version",
                    "v0.9.3",
                    "--assets",
                    str(assets),
                    "--out",
                    str(out),
                ],
                check=True,
            )
            mode = (out / "wiremux-installer.sh").stat().st_mode
            self.assertTrue(mode & stat.S_IXUSR)
            subprocess.run(["sh", "-n", str(out / "wiremux-installer.sh")], check=True)
            check = subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/render_release_extras.py"),
                    "--version",
                    "0.9.3",
                    "--assets",
                    str(assets),
                    "--out",
                    str(out),
                    "--check",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(check.returncode, 0, check.stderr)
            (out / "bucket/wiremux.json").write_text("{}\n", encoding="utf-8")
            stale = subprocess.run(
                [
                    "python3",
                    str(ROOT / "scripts/render_release_extras.py"),
                    "--version",
                    "0.9.3",
                    "--assets",
                    str(assets),
                    "--out",
                    str(out),
                    "--check",
                ],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("stale:", stale.stderr)

    def test_push_script_skips_without_token(self) -> None:
        result = subprocess.run(
            ["bash", str(ROOT / "scripts/push-package-indexes.sh")],
            check=False,
            capture_output=True,
            text=True,
            env={
                "PATH": os.environ.get("PATH", ""),
                "TAG": "v0.9.3",
                "GH_REPO": "wiremuxhq/wiremux",
                "ASSET_DIR": "/tmp",
                "TOKEN": "",
            },
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("HOMEBREW_TAP_TOKEN unset", result.stdout)


if __name__ == "__main__":
    unittest.main()
