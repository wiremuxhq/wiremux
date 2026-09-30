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
    def test_stealth_required_check_runs_assert_public(self) -> None:
        text = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("name: Stealth", text)
        self.assertIn("scripts/assert-public.sh", text)
        self.assertNotIn("README must be Not ready.", text)

    def test_ci_has_no_push_compile(self) -> None:
        on_block = _on_block((WORKFLOWS / "ci.yml").read_text(encoding="utf-8"))
        self.assertIn("pull_request:", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("push:", on_block)
        self.assertNotIn("tags:", on_block)

    def test_ci_covers_client_and_maps_only_response_maps(self) -> None:
        text = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("--features client", text)
        self.assertIn("--test response_maps", text)
        self.assertIn("--test client", text)

    def test_ci_runs_release_script_unit_tests(self) -> None:
        text = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertIn("python3 scripts/test_package_release_binary.py", text)
        self.assertIn("python3 scripts/test_render_release_extras.py", text)
        self.assertIn("python3 scripts/test_upload_release_binaries.py", text)
        self.assertIn("release-please-config.json", text)
        self.assertIn("- 'fuzz/**'", text)
        self.assertIn(
            "github.event.pull_request.user.login == 'github-actions[bot]'",
            text,
        )
        self.assertIn(
            "github.event.pull_request.user.login != 'github-actions[bot]'",
            text,
        )
        self.assertNotIn("github.actor == 'github-actions[bot]'", text)

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
        self.assertIn("docs/CONSUME.md", text)

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
        self.assertIn("git add fuzz/Cargo.lock", workflow)
        self.assertIn("toolchain-file: release/rust-toolchain.toml", workflow)
        self.assertIn("attestations: write", workflow)
        self.assertIn("cargo check --manifest-path fuzz/Cargo.toml", script)
        self.assertIn("publish-crates:", workflow)
        self.assertIn("release_created == 'true'", workflow)
        self.assertIn("apply-release-notes:", workflow)
        self.assertIn("scripts/apply-release-notes.sh", workflow)
        self.assertIn("uses: ./.github/workflows/publish-crates.yml", workflow)
        self.assertIn("dispatch-release-binaries:", workflow)
        self.assertIn('gh workflow run "Release binaries"', workflow)
        self.assertIn("actions: write", workflow)
        self.assertNotIn("crates-io-auth-action", workflow)
        self.assertIn("tag_name", workflow)

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

    def test_path_dep_sync_keeps_default_features_off(self) -> None:
        import subprocess
        import sys
        import tempfile

        tmp = Path(tempfile.mkdtemp())
        (tmp / "crates/wiremux-auth").mkdir(parents=True)
        (tmp / "crates/wiremux").mkdir(parents=True)
        (tmp / "crates/wiremux-auth/Cargo.toml").write_text(
            '[package]\nname = "wiremux-auth"\nversion = "0.6.0"\n',
            encoding="utf-8",
        )
        pin = tmp / "crates/wiremux/Cargo.toml"
        pin.write_text(
            'wiremux-auth = { version = "0.5.0", path = "../wiremux-auth", default-features = false }\n',
            encoding="utf-8",
        )
        out = subprocess.check_output(
            [
                sys.executable,
                str(ROOT / "scripts/sync-path-dep-versions.py"),
                str(tmp),
            ],
            text=True,
        )
        self.assertIn("0.6.0", out)
        self.assertEqual(
            pin.read_text(encoding="utf-8"),
            'wiremux-auth = { version = "0.6.0", path = "../wiremux-auth", default-features = false }\n',
        )

    def test_auto_merge_skips_release_please_head(self) -> None:
        text = (WORKFLOWS / "auto-approve.yml").read_text(encoding="utf-8")
        self.assertIn("!startsWith(github.head_ref, 'release-please')", text)
        self.assertIn("autorelease: pending", text)
        self.assertIn("Skip hmarr on release-please", text)

    def test_auto_merge_skips_constitution_and_public_surface_scripts(self) -> None:
        text = (WORKFLOWS / "auto-approve.yml").read_text(encoding="utf-8")
        self.assertIn("CONSTITUTION.md", text)
        self.assertIn("scripts/assert-stealth.sh", text)
        self.assertIn("scripts/assert-public.sh", text)
        self.assertIn("constitution or public-surface script in diff", text)

    def test_cheap_pr_status_checks_do_not_cancel(self) -> None:
        for name in (
            "pr-title.yml",
            "dco.yml",
            "auto-approve.yml",
            "dependabot-auto-merge.yml",
        ):
            text = (WORKFLOWS / name).read_text(encoding="utf-8")
            self.assertIn("cancel-in-progress: false", text, name)

    def test_publish_crates_is_tag_or_dispatch(self) -> None:
        text = (WORKFLOWS / "publish-crates.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertIn("workflow_call:", on_block)
        self.assertIn("tags:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertNotIn("branches:", on_block)
        self.assertIn("id-token: write", text)
        self.assertIn("crates-io-auth-action", text)
        self.assertNotIn("CARGO_REGISTRY_TOKEN: ${{ secrets.", text)
        self.assertIn("scripts/publish-crates.sh", text)
        self.assertIn("path: publisher", text)
        self.assertIn("path: crate", text)
        self.assertNotIn("cargo publish -p wiremux-auth", text)
        self.assertNotIn("Refuse while unpublished", text)
        self.assertNotIn("publish = false on crates", text)
        script = (ROOT / "scripts" / "publish-crates.sh").read_text(encoding="utf-8")
        self.assertIn("wiremux-auth", script)
        self.assertIn("wiremux", script)
        self.assertIn("already on crates.io", script)
        self.assertIn("already uploaded", script)
        self.assertIn("cargo publish --locked -p", script)
        self.assertIn("github.event_name != 'push'", text)
        self.assertIn("github.event.created", text)
        self.assertIn("inputs.tag != ''", text)
        header = text.split("\njobs:", 1)[0]
        self.assertIn("contents: read", header)
        self.assertNotIn("contents: write", header)
        self.assertNotIn("id-token:", header)
        job = text.split("\njobs:", 1)[1]
        self.assertIn("contents: write", job)
        self.assertIn("id-token: write", job)
        self.assertNotIn("github.event_name == 'workflow_call'", text)
        self.assertIn("attestations: write", text)
        self.assertIn(
            "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8",
            text,
        )
        self.assertIn("scripts/package-release-crates.sh", text)
        self.assertIn("scripts/upload-release-assets.sh", text)
        self.assertNotIn('TAG="${TAG}"', text)
        package = (ROOT / "scripts" / "package-release-crates.sh").read_text(
            encoding="utf-8"
        )
        upload = (ROOT / "scripts" / "upload-release-assets.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("cargo package --locked -p", package)
        self.assertIn("package_one wiremux-auth auth_crate", package)
        self.assertIn("package_one wiremux wiremux_crate", package)
        self.assertIn(".intoto.jsonl", upload)
        self.assertIn("gh release upload", upload)
        self.assertNotIn('TAG="${TAG}"', package)
        self.assertNotIn('TAG="${TAG}"', upload)

    def test_gitleaks_tarball_is_sha_pinned(self) -> None:
        text = (WORKFLOWS / "security.yml").read_text(encoding="utf-8")
        self.assertIn(
            "9991e0b2903da4c8f6122b5c3186448b927a5da4deef1fe45271c3793f4ee29c",
            text,
        )
        self.assertIn("sha256sum -c -", text)

    def test_scorecard_and_link_check_are_cheap(self) -> None:
        scorecard = (WORKFLOWS / "scorecard.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", scorecard)
        self.assertIn("ossf/scorecard-action@", scorecard)
        self.assertNotRegex(scorecard, r"cargo (test|nextest|clippy|fuzz)")
        links = (WORKFLOWS / "link-check.yml").read_text(encoding="utf-8")
        self.assertIn("lycheeverse/lychee-action@", links)
        self.assertIn("workflow_dispatch:", links)
        self.assertIn("fail: ${{ github.event_name != 'pull_request' }}", links)
        self.assertIn("scripts/report-scheduled-failure.py", scorecard)
        self.assertIn("Scorecard red", scorecard)
        self.assertIn("scripts/report-scheduled-failure.py", links)
        self.assertIn("Link check red", links)
        lychee = (ROOT / "lychee.toml").read_text(encoding="utf-8")
        self.assertIn("exclude_path = [\"CHANGELOG.md\"]", lychee)
        self.assertNotIn("blineai/bline", lychee)

    def test_apply_release_notes_dry_run(self) -> None:
        import subprocess
        import tempfile

        notes = Path(tempfile.mkdtemp()) / "RELEASE_NOTES.md"
        notes.write_text("# wiremux 0.2.1\n", encoding="utf-8")
        out = subprocess.check_output(
            ["bash", str(ROOT / "scripts" / "apply-release-notes.sh")],
            env={
                "TAG": "v0.2.1",
                "GH_REPO": "wiremuxhq/wiremux",
                "DRY_RUN": "1",
                "NOTES_FILE": str(notes),
                "PATH": __import__("os").environ.get("PATH", ""),
            },
            text=True,
        )
        self.assertIn("DRY_RUN: would apply file:", out)
        apply_wf = (WORKFLOWS / "apply-release-notes.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", apply_wf)
        self.assertNotIn("pull_request:", apply_wf)
        self.assertIn("scripts/apply-release-notes.sh", apply_wf)

    def test_nightly_smoke_is_schedule_or_dispatch(self) -> None:
        text = (WORKFLOWS / "nightly-smoke.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("schedule:", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertNotIn("push:", on_block)
        self.assertIn("cargo fuzz", text)
        self.assertIn("--target x86_64-unknown-linux-gnu", text)
        self.assertNotIn("no live vendor secrets; skip", text)
        self.assertIn("github.event_name == 'schedule'", text)
        self.assertIn("scripts/report-scheduled-failure.py", text)
        self.assertIn("Nightly fuzz red", text)
        self.assertIn('wait "$pid_sse"', text)
        self.assertIn('wait "$pid_es"', text)

    def test_msrv_is_1_95(self) -> None:
        toolchain = (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertIn('channel = "1.95"', toolchain)
        ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertNotIn('toolchain: "1.95"', ci)
        self.assertNotIn('toolchain: "1.85"', ci)
        self.assertNotIn("1.85", ci)
        self.assertIn("./.github/actions/rust-ci", ci)
        publish = (WORKFLOWS / "publish-crates.yml").read_text(encoding="utf-8")
        self.assertNotIn('toolchain: "1.95"', publish)
        self.assertNotIn("1.85", publish)
        self.assertIn("toolchain-file: crate/rust-toolchain.toml", publish)
        cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertIn('rust-version = "1.95"', cargo)
        action = (ROOT / ".github" / "actions" / "rust-ci" / "action.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("RUSTC_WRAPPER=sccache", action)
        self.assertIn("SCCACHE_GHA_ENABLED=true", action)
        self.assertIn("sccache@0.18.0", action)
        self.assertIn(
            "actions/github-script@ed597411d8f924073f98dfc5c65a23a2325f34cd",
            action,
        )
        self.assertIn("ACTIONS_RESULTS_URL", action)
        self.assertIn("ACTIONS_RUNTIME_TOKEN", action)
        self.assertLess(
            action.index("ACTIONS_RESULTS_URL"),
            action.index("SCCACHE_GHA_ENABLED=true"),
        )
        self.assertNotIn('toolchain: "1.95"', action)
        release = (WORKFLOWS / "release-please.yml").read_text(encoding="utf-8")
        self.assertNotIn('toolchain: "1.95"', release)

    def test_dco_rejects_anthropic_and_claude_session(self) -> None:
        text = (WORKFLOWS / "dco.yml").read_text(encoding="utf-8")
        self.assertIn("name: DCO", text)
        self.assertIn("body=$(git log -1 --format='%B' \"$sha\")", text)
        self.assertIn(
            "grep -qiE '^[[:space:]]*Co-authored-by:.*anthropic\\.com'",
            text,
        )
        self.assertIn("grep -qiE '^[[:space:]]*Claude-Session:'", text)

    def test_dependabot_auto_merge_skips_majors_and_does_not_checkout(self) -> None:
        text = (WORKFLOWS / "dependabot-auto-merge.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("pull_request_target:", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("actions/checkout", text)
        self.assertIn(
            "dependabot/fetch-metadata@25dd0e34f4fe68f24cc83900b1fe3fe149efef98",
            text,
        )
        self.assertIn("version-update:semver-major", text)
        self.assertIn(
            "github.event.pull_request.user.login == 'dependabot[bot]'",
            text,
        )
        self.assertIn(
            "Skipping major version update. It needs manual review.",
            text,
        )
        self.assertNotIn("\u2014", text)
        self.assertIn("gh pr merge", text)
        self.assertIn("--auto", text)
        self.assertIn("--squash", text)

    def test_path_filters_treat_skipped_as_success(self) -> None:
        ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
        self.assertIn(
            "dorny/paths-filter@ceb8a2b8f2d89434be7ff52d3de7ec3738c5cc9d",
            ci,
        )
        self.assertIn("needs.changes.result == 'skipped'", ci)
        self.assertIn("success|skipped", ci)
        self.assertIn("name: Lint run", ci)
        self.assertIn("name: Lint", ci)
        self.assertIn("name: Workflow lint", ci)
        self.assertIn("name: Workflows", ci)
        self.assertIn('os=["ubuntu-latest"]', ci)
        self.assertIn('os=["ubuntu-latest","macos-latest","windows-latest"]', ci)
        self.assertIn('[[ "$HEAD_REF" == release-please* ]]', ci)
        self.assertIn('[ "$ACTOR" = "github-actions[bot]" ]', ci)
        self.assertNotIn("github.event_name == 'push'", ci)
        security = (WORKFLOWS / "security.yml").read_text(encoding="utf-8")
        self.assertIn("name: CodeQL (rust)", security)
        self.assertIn("name: CodeQL (actions)", security)
        self.assertIn("name: CodeQL rust analysis", security)
        self.assertIn("name: CodeQL actions analysis", security)
        self.assertIn("name: Dependency review", security)
        self.assertIn(
            "version-bump PR; CodeQL already ran on the feature PR",
            security,
        )
        self.assertIn("Monday security red", security)
        self.assertIn("scripts/report-scheduled-failure.py", security)
        self.assertIn('cron: "17 4 * * 1"', security)

    def test_pr_title_reports_on_merge_group(self) -> None:
        text = (WORKFLOWS / "pr-title.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("merge_group:", on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertIn("name: Semantic PR Title", text)
        self.assertIn("semantic title already passed on the pull request", text)
        self.assertIn("dependabot pull requests are exempt", text)
        self.assertIn("github.event_name == 'pull_request'", text)

    def test_security_codeql_runs_on_main_push(self) -> None:
        text = (WORKFLOWS / "security.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("push:", on_block)
        self.assertIn("branches: [main]", on_block)
        self.assertIn('cron: "17 4 * * 1"', on_block)
        self.assertNotIn("cargo test", text)
        self.assertNotIn("cargo clippy", text)

    def test_stale_exempts_maintainer_labels(self) -> None:
        text = (WORKFLOWS / "stale.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertIn("schedule:", on_block)
        self.assertIn(
            "actions/stale@4391f3da665fdf50b6810c1a66712fb9ba21aa93",
            text,
        )
        self.assertIn("good first issue,help wanted,constitution,security", text)
        self.assertIn("autorelease: pending", text)
        self.assertIn("timeout-minutes: 10", text)
        self.assertIn("cancel-in-progress: false", text)
        self.assertNotIn("\u2014", text)

    def test_reporter_requires_args(self) -> None:
        import subprocess
        import sys

        script = ROOT / "scripts" / "report-scheduled-failure.py"
        proc = subprocess.run(
            [sys.executable, str(script)],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(proc.returncode, 2, proc.stderr)

    def test_reporter_decide(self) -> None:
        import importlib.util
        import json
        from datetime import date

        path = ROOT / "scripts" / "report-scheduled-failure.py"
        spec = importlib.util.spec_from_file_location("report_scheduled_failure", path)
        self.assertIsNotNone(spec)
        assert spec is not None and spec.loader is not None
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)

        today = date(2026, 9, 29)
        yesterday = date(2026, 9, 28).isoformat()
        red = {"fuzz-smoke": "failure"}
        green = {"fuzz-smoke": "success"}
        issue = {
            "signature": "fuzz-smoke",
            "first_failed_on": today.isoformat(),
            "run_id": "10",
        }

        self.assertEqual(
            mod.decide(today=today, results={"fuzz-smoke": "cancelled"}, issue=issue, run_id="11"),
            {"action": "noop"},
        )
        self.assertEqual(
            mod.decide(today=today, results=green, issue=None, run_id="11"),
            {"action": "noop"},
        )
        self.assertEqual(
            mod.decide(today=today, results=green, issue=issue, run_id="11")["action"],
            "close",
        )
        created = mod.decide(today=today, results=red, issue=None, run_id="11")
        self.assertEqual(created["action"], "create")
        self.assertEqual(created["day_count"], 1)
        self.assertEqual(created["first_failed_on"], today.isoformat())
        self.assertEqual(
            mod.decide(today=today, results=red, issue=issue, run_id="10")["action"],
            "noop",
        )
        updated = mod.decide(today=today, results=red, issue=issue, run_id="12")
        self.assertEqual(updated["action"], "update")
        self.assertEqual(updated["day_count"], 1)
        self.assertEqual(updated["first_failed_on"], today.isoformat())
        next_day = mod.decide(
            today=today,
            results=red,
            issue={
                "signature": "fuzz-smoke",
                "first_failed_on": yesterday,
                "run_id": "9",
            },
            run_id="13",
        )
        self.assertEqual(next_day["action"], "replace")
        self.assertEqual(next_day["day_count"], (today - date(2026, 9, 28)).days + 1)
        self.assertEqual(next_day["first_failed_on"], yesterday)
        changed = mod.decide(
            today=today,
            results={"gitleaks": "failure"},
            issue=issue,
            run_id="14",
        )
        self.assertEqual(changed["action"], "replace")
        self.assertEqual(changed["day_count"], 1)
        self.assertEqual(changed["first_failed_on"], today.isoformat())
        unmarked = [{"number": 4, "title": "Nightly fuzz red: old", "body": "no marker"}]
        green_issue, _ordered = mod.prepare_matches(unmarked, results=green, today=today)
        self.assertIsNotNone(green_issue)
        assert green_issue is not None
        self.assertEqual(
            mod.decide(today=today, results=green, issue=green_issue, run_id="15")["action"],
            "close",
        )
        red_issue, _ordered = mod.prepare_matches(unmarked, results=red, today=today)
        self.assertIsNone(red_issue)
        body = mod.render_body(
            run_url="https://example.test/run/1",
            signature="fuzz-smoke",
            day_count_value=2,
            first_failed_on=yesterday,
            run_id="13",
        )
        self.assertNotIn("\u2014", body)
        self.assertNotIn("boring", body)
        self.assertNotIn("honest", body)
        self.assertEqual(
            mod.parse_state(body),
            {
                "signature": "fuzz-smoke",
                "first_failed_on": yesterday,
                "run_id": "13",
            },
        )
        dry = __import__("subprocess").run(
            [
                __import__("sys").executable,
                str(path),
                "--prefix",
                "Nightly fuzz red",
                "--run-id",
                "11",
                "--repo",
                "wiremuxhq/wiremux",
                "--today",
                today.isoformat(),
                "--dry-run",
            ],
            capture_output=True,
            text=True,
            check=False,
            env={
                "PATH": __import__("os").environ.get("PATH", ""),
                "JOB_RESULTS": "fuzz-smoke=failure",
            },
        )
        self.assertEqual(dry.returncode, 0, dry.stderr)
        payload = json.loads(dry.stdout.strip().splitlines()[-2])
        self.assertEqual(payload["action"], "create")

    def test_release_binaries_upload_portable_archives(self) -> None:
        text = (WORKFLOWS / "release-binaries.yml").read_text(encoding="utf-8")
        on_block = _on_block(text)
        self.assertIn("push:", on_block)
        self.assertIn('tags:', on_block)
        self.assertIn('"v[0-9]+.[0-9]+.[0-9]+"', on_block)
        self.assertIn("workflow_dispatch:", on_block)
        self.assertNotIn("pull_request:", on_block)
        self.assertIn("x86_64-unknown-linux-gnu", text)
        self.assertIn("aarch64-unknown-linux-gnu", text)
        self.assertIn("ubuntu-24.04-arm", text)
        self.assertIn("aarch64-apple-darwin", text)
        self.assertIn("x86_64-apple-darwin", text)
        self.assertIn("macos-15-intel", text)
        self.assertIn("x86_64-pc-windows-msvc", text)
        self.assertIn("scripts/package_release_binary.py", text)
        self.assertIn("scripts/upload-release-binaries.sh", text)
        self.assertIn("scripts/render_release_extras.py", text)
        self.assertIn("scripts/push-package-indexes.sh", text)
        self.assertIn("cargo-cyclonedx --version 0.5.9", text)
        self.assertIn("wiremux-sbom.cdx.json", text)
        self.assertIn(".intoto.jsonl", text)
        self.assertIn(
            "actions/attest-build-provenance@4d101475d8b20a2381f78447822ac1eab6504dd8",
            text,
        )
        self.assertIn("name: Checkout workflow", text)
        self.assertIn("ref: ${{ github.sha }}", text)
        self.assertIn("path: crate", text)
        self.assertIn("working-directory: crate", text)
        self.assertIn("crate/target/release/wiremux", text)
        self.assertIn("contents: write", text)
        self.assertIn("attestations: write", text)
        self.assertIn("id-token: write", text)
        self.assertIn("github.event.created", text)
        self.assertIn("inputs.tag != ''", text)
        self.assertIn("HOMEBREW_TAP_TOKEN", text)
        self.assertIn("WINGET_TOKEN unset", text)
        self.assertIn("CHOCOLATEY_API_KEY unset", text)
        self.assertIn("Wiremux.Wiremux", text)
        self.assertIn("fork-user: SebTardif", text)
        self.assertIn("continue-on-error: true", text)
        self.assertIn(
            "vedantmgoyal9/winget-releaser@4ffc7888bffd451b357355dc214d43bb9f23917e",
            text,
        )
        self.assertNotIn("cargo publish", text)
        self.assertNotIn('TAG="${TAG}"', text)
        header = text.split("\njobs:", 1)[0]
        self.assertIn("contents: read", header)
        self.assertNotIn("contents: write", header)
        self.assertNotIn("id-token:", header)
        self.assertNotIn("attestations:", header)
        push = (ROOT / "scripts" / "push-package-indexes.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("HOMEBREW_TAP_TOKEN unset", push)
        upload = (ROOT / "scripts" / "upload-release-binaries.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("duplicate asset names", upload)
        self.assertIn(".intoto.jsonl", text)

    def test_windows_packages_install_current_release(self) -> None:
        text = (WORKFLOWS / "windows-packages.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", text)
        self.assertIn("runs-on: windows-latest", text)
        self.assertIn("timeout-minutes: 20", text)
        self.assertIn("https://get.scoop.sh", text)
        self.assertIn("-RunAsAdmin", text)
        self.assertIn("https://github.com/wiremuxhq/scoop-bucket", text)
        self.assertIn("scoop install wiremux", text)
        self.assertIn("scoop uninstall wiremux", text)
        self.assertIn("choco pack .\\wiremux.nuspec", text)
        self.assertIn("choco install wiremux --source $dir -y --version $version", text)
        self.assertIn("choco uninstall wiremux -y", text)
        self.assertIn("gh release view --repo wiremuxhq/wiremux", text)
        self.assertNotIn("0.9.3", text)
        self.assertNotIn("CHOCOLATEY_API_KEY", text)
        self.assertIn(
            "step-security/harden-runner@e14015d583714f6e62063499dc959a02595150a1",
            text,
        )

    def test_publish_chocolatey_pushes_current_release(self) -> None:
        text = (WORKFLOWS / "publish-chocolatey.yml").read_text(encoding="utf-8")
        self.assertIn("workflow_dispatch:", text)
        self.assertIn("runs-on: windows-latest", text)
        self.assertIn("timeout-minutes: 20", text)
        self.assertIn("cancel-in-progress: false", text)
        self.assertIn("secrets.CHOCOLATEY_API_KEY", text)
        self.assertIn("CHOCOLATEY_API_KEY is empty", text)
        self.assertIn("https://push.chocolatey.org/", text)
        self.assertIn("choco pack .\\wiremux.nuspec", text)
        self.assertIn("GetElementsByTagName('version')", text)
        self.assertIn("choco push", text)
        self.assertIn("gh release view --repo wiremuxhq/wiremux", text)
        self.assertNotIn("0.9.3", text)
        self.assertIn(
            "step-security/harden-runner@e14015d583714f6e62063499dc959a02595150a1",
            text,
        )


if __name__ == "__main__":
    unittest.main()
