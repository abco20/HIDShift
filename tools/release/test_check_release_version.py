from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from check_release_version import MANIFESTS, validate


class ReleaseVersionValidationTests(unittest.TestCase):
    def make_repository(self, version: str = "1.2.3") -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for manifest in MANIFESTS:
            path = root / manifest
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f'[package]\nname = "test"\nversion = "{version}"\n', encoding="utf-8")
        (root / "CHANGELOG.md").write_text(f"# Changelog\n\n## {version} - 2026-01-01\n", encoding="utf-8")
        return root

    def test_accepts_stable_and_prerelease_tags(self) -> None:
        root = self.make_repository()
        self.assertEqual(validate(root, "v1.2.3"), [])
        self.assertEqual(validate(root, "v1.2.3-rc.1"), [])

    def test_rejects_invalid_tag(self) -> None:
        self.assertTrue(validate(self.make_repository(), "release-1.2.3"))

    def test_reports_manifest_and_changelog_mismatches(self) -> None:
        root = self.make_repository("1.2.2")
        errors = validate(root, "v1.2.3")
        self.assertEqual(len(errors), len(MANIFESTS) + 1)


if __name__ == "__main__":
    unittest.main()
