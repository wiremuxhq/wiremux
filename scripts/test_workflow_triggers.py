#!/usr/bin/env python3
"""Lock Recipe A and the cheap release-please split."""

from __future__ import annotations

import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"


def _on_block(text: str) -> str:
    start = text.index("\non:")
    rest = text[start + 1 :]
    end = rest.index("\njobs:")
    block = rest[:end]
    lines = []
    for line in block.splitlines():
        stripped = line.split("#", 1)[0].rstrip()
        if stripped:
            lines.append(stripped)
    return "\n".join(lines)


class WorkflowTriggerTests(unittest.TestCase):
    def test_ci_has_no_push_compile(self) -> None:
        on_block = _on_block((WORKFLOWS / "ci.yml").read_text(encoding="utf-8"))
        self.assertIn("pull_request:", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("push:", on_block)
        self.assertNotIn("tags:", on_block)

    def test_actionlint_is_not_an_install_action_tool(self) -> None:
        text = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertNotIn("tool: actionlint@", text)
        self.assertIn("rhysd/actionlint@", text)

    def test_gitleaks_is_not_an_install_action_tool(self) -> None:
        text = (WORKFLOWS / "security.yml").read_text(encoding="utf-8")
        self.assertNotIn("tool: gitleaks@", text)
        self.assertIn("gitleaks/gitleaks/releases/download/", text)

    def test_release_please_uses_simple_for_virtual_workspace(self) -> None:
        text = (ROOT / "release-please-config.json").read_text(encoding="utf-8")
        self.assertIn('"release-type": "simple"', text)
        self.assertNotIn('"release-type": "rust"', text)
        self.assertIn("crates/wiremux/Cargo.toml", text)
        self.assertIn("crates/wiremux-auth/Cargo.toml", text)
        self.assertIn("$.dependencies.wiremux-auth.version", text)

    def test_release_please_is_main_only(self) -> None:
        text = (WORKFLOWS / "release-please.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("push:", on_block)
        self.assertIn("branches: [main]", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertNotRegex(text, r"cargo (test|nextest|clippy|fuzz)")
        self.assertIn("sync-release-pr-versions:", text)
        self.assertIn("scripts/sync-cargo-lock-workspace-versions.sh", text)
        self.assertIn("path: publisher", text)
        self.assertIn("path: release", text)
        self.assertIn("SYNC_ROOT", text)
        self.assertIn("autorelease: pending", text)
        self.assertNotIn("needs.release-please.outputs.pr != ''", text)
        script = (ROOT / "scripts" / "sync-cargo-lock-workspace-versions.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn('ROOT="${SYNC_ROOT:-', script)
        self.assertIn("cargo check -p wiremux", script)
        self.assertIn("sync path-dep wiremux-auth version", script)
        self.assertIn("sync-path-dep-versions.py", script)
        self.assertNotIn("cargo generate-lockfile", script)
        workflow = (WORKFLOWS / "release-please.yml").read_text(encoding="utf-8")
        self.assertIn("git add Cargo.lock crates/wiremux/Cargo.toml", workflow)

    def test_path_dep_sync_rewrites_stale_pin(self) -> None:
        import subprocess
        import sys
        import tempfile

        tmp = Path(tempfile.mkdtemp())
        (tmp / "crates/wiremux-auth").mkdir(parents=True)
        (tmp / "crates/wiremux").mkdir(parents=True)
        (tmp / "crates/wiremux-auth/Cargo.toml").write_text(
            '[package]\nname = "wiremux-auth"\nversion = "0.2.0"\n',
            encoding="utf-8",
        )
        pin = tmp / "crates/wiremux/Cargo.toml"
        pin.write_text(
            'wiremux-auth = { version = "0.1.0", path = "../wiremux-auth" }\n',
            encoding="utf-8",
        )
        out = subprocess.check_output(
            [
                sys.executable,
                str(ROOT / "scripts" / "sync-path-dep-versions.py"),
                str(tmp),
            ],
            text=True,
        )
        self.assertIn("0.2.0", out)
        self.assertIn(
            'wiremux-auth = { version = "0.2.0", path = "../wiremux-auth" }',
            pin.read_text(encoding="utf-8"),
        )

    def test_auto_merge_skips_release_please_head(self) -> None:
        text = (WORKFLOWS / "auto-approve.yml").read_text(encoding="utf-8")
        self.assertIn("!startsWith(github.head_ref, 'release-please')", text)
        self.assertIn("autorelease: pending", text)
        self.assertIn("Skip hmarr on release-please", text)

    def test_cheap_pr_status_checks_do_not_cancel(self) -> None:
        for name in ("pr-title.yml", "dco.yml"):
            text = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertIn("cancel-in-progress: false", text, name)

    def test_publish_crates_is_tag_or_dispatch(self) -> None:
        text = (WORKFLOWS / "publish-crates.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertIn("tags:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertNotIn("branches:", on_block)
        self.assertIn("id-token: write", text)
        self.assertIn("crates-io-auth-action", text)
        self.assertNotIn("CARGO_REGISTRY_TOKEN: ${{ secrets.", text)
        self.assertIn("cargo publish -p wiremux-auth", text)
        self.assertIn("cargo publish -p wiremux", text)
        self.assertNotIn("Refuse while unpublished", text)
        self.assertNotIn("publish = false on crates", text)

    def test_msrv_is_1_95(self) -> None:
        toolchain = (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertIn('channel = "1.95"', toolchain)
        ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertIn('toolchain: "1.95"', ci)
        self.assertNotIn('toolchain: "1.85"', ci)
        publish = (WORKFLOWS / "publish-crates.yml").read_text(encoding="utf-8")
        self.assertIn('toolchain: "1.95"', publish)
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('rust-version = "1.95"', cargo)


if __name__ == "__main__":
    unittest.main()
