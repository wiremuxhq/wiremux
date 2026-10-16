#!/usr/bin/env python3
"""cargo-semver-checks rejects a new enum field and allows IrSampling."""

from __future__ import annotations

import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

BASELINE_LIB = """\
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Sampling {
    pub temperature: Option<f32>,
}

impl Sampling {
    /// Hosts start from Default and leave new fields unset.
    pub fn patch(update: impl FnOnce(&mut Self)) -> Self {
        let mut sampling = Self::default();
        update(&mut sampling);
        sampling
    }
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Item {
    System { text: String },
}
"""

ENUM_LIB = BASELINE_LIB.replace(
    "System { text: String }",
    "System { text: String, extra: Option<String> }",
)

SAMPLING_LIB = BASELINE_LIB.replace(
    "pub temperature: Option<f32>,",
    "pub temperature: Option<f32>,\n    pub top_k: Option<u32>,",
)


def _write_crate(path: Path, version: str, lib: str) -> None:
    path.mkdir(parents=True)
    (path / "src").mkdir()
    (path / "Cargo.toml").write_text(
        "\n".join(
            [
                "[package]",
                'name = "semver-fixture"',
                f'version = "{version}"',
                'edition = "2021"',
                "publish = false",
                "",
            ]
        ),
        encoding="utf-8",
    )
    (path / "src" / "lib.rs").write_text(lib, encoding="utf-8")


def _check(current: Path, baseline: Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "cargo",
            "semver-checks",
            "check-release",
            "--baseline-root",
            str(baseline),
            "--manifest-path",
            str(current / "Cargo.toml"),
        ],
        check=False,
        capture_output=True,
        text=True,
    )


class SemverLintTests(unittest.TestCase):
    def test_enum_field_fails_and_sampling_field_passes(self) -> None:
        if shutil.which("cargo-semver-checks") is None and shutil.which("cargo") is None:
            self.fail("cargo semver-checks is not installed")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            baseline = root / "baseline"
            enum_field = root / "enum-field"
            sampling = root / "sampling-field"
            _write_crate(baseline, "0.11.3", BASELINE_LIB)
            _write_crate(enum_field, "0.11.4", ENUM_LIB)
            _write_crate(sampling, "0.11.4", SAMPLING_LIB)
            broken = _check(enum_field, baseline)
            self.assertNotEqual(
                broken.returncode,
                0,
                f"enum field must fail\n{broken.stdout}\n{broken.stderr}",
            )
            broken_text = f"{broken.stdout}\n{broken.stderr}"
            self.assertIn("enum_struct_variant_field_added", broken_text)
            ok = _check(sampling, baseline)
            self.assertEqual(
                ok.returncode,
                0,
                f"IrSampling-shaped field must pass\n{ok.stdout}\n{ok.stderr}",
            )
            self.assertNotIn("constructible_struct_adds_field", f"{ok.stdout}\n{ok.stderr}")


if __name__ == "__main__":
    unittest.main()
