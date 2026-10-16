#!/usr/bin/env python3
"""The Semver baseline is the latest v0.11 tag, not the pull request base."""

from __future__ import annotations

import unittest
from pathlib import Path

from semver_baseline import latest_v011

ROOT = Path(__file__).resolve().parents[1]


class SemverBaselineTests(unittest.TestCase):
    def test_latest_patch_wins_over_an_older_tag_and_a_peel(self) -> None:
        lines = [
            "aaa refs/tags/v0.11.2",
            "bbb refs/tags/v0.11.3",
            "ccc refs/tags/v0.11.3^{}",
            "ddd refs/tags/v0.11.3-rc1",
            "eee refs/tags/v0.10.9",
            "fff refs/tags/v0.12.0",
        ]
        self.assertEqual(latest_v011(lines), "v0.11.3")

    def test_missing_line_is_none(self) -> None:
        self.assertIsNone(latest_v011(["aaa refs/tags/v0.10.1"]))

    def test_check_script_does_not_use_the_pull_request_base(self) -> None:
        text = (ROOT / "scripts" / "check-semver.sh").read_text(encoding="utf-8")
        self.assertIn("v0.11.*", text)
        self.assertIn("semver_baseline.py", text)
        self.assertNotIn("github.base_ref", text)
        self.assertNotIn("github.sha", text)
        self.assertNotIn("pull_request.base", text)
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("name: Semver", workflow)
        self.assertIn("cargo-semver-checks@0.51.0", workflow)
        self.assertIn("bash scripts/check-semver.sh", workflow)
        self.assertIn("python3 scripts/test_semver_lints.py", workflow)


if __name__ == "__main__":
    unittest.main()
