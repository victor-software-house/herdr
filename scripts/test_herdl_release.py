import argparse
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from scripts import herdl_release


class HerDLReleaseManifestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.artifacts = self.root / "artifacts"
        self.notes = self.root / "notes.md"
        self.notes.write_text("### Changed\n- VSH build\n", encoding="utf-8")
        for target in herdl_release.TARGETS:
            filename = f"herdl-{target}"
            directory = self.artifacts / filename
            directory.mkdir(parents=True)
            artifact = directory / filename
            artifact.write_bytes(target.encode())
            digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
            (directory / f"{filename}.sha256").write_text(
                f"{digest}  {filename}\n", encoding="utf-8"
            )

    def tearDown(self):
        self.temp.cleanup()

    def args(self, **overrides):
        values = {
            "base_version": "0.9.0",
            "build_id": "abcdef123456",
            "commit": "abcdef1234567890abcdef1234567890abcdef12",
            "built_at": "2026-09-13T12:00:00Z",
            "protocol": 23,
            "endpoint_generation": 1,
            "notes": self.notes,
            "artifacts_dir": self.artifacts,
            "previous_latest": None,
            "previous_preview": None,
            "latest_output": self.root / "latest.json",
            "preview_output": self.root / "preview.json",
            "retain": 30,
        }
        values.update(overrides)
        return argparse.Namespace(**values)

    def test_manifests_publish_only_vsh_herdl_unix_assets(self):
        latest, preview = herdl_release.generate_manifests(self.args())

        self.assertEqual(latest["version"], "0.9.0")
        self.assertEqual(latest["identity"], "0.9.0-vsh.abcdef123456")
        self.assertEqual(preview["identity"], latest["identity"])
        self.assertEqual(set(latest["assets"]), set(herdl_release.TARGETS))
        self.assertNotIn("windows-x86_64", latest["assets"])
        for target, asset in latest["assets"].items():
            self.assertEqual(
                asset["url"],
                "https://github.com/victor-software-house/herdr/releases/"
                f"download/v0.9.0-vsh.abcdef123456/herdl-{target}",
            )
            self.assertEqual(len(asset["sha256"]), 64)
            self.assertNotIn("herdrdev/herdr", asset["url"])
            self.assertFalse(asset["url"].endswith(f"herdr-{target}"))

    def test_manifest_rejects_checksum_mismatch(self):
        checksum = next(self.artifacts.glob("*/*.sha256"))
        checksum.write_text("0" * 64 + "  " + checksum.stem + "\n", encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            herdl_release.generate_manifests(self.args())

    def test_manifest_retains_previous_exact_build(self):
        first, first_preview = herdl_release.generate_manifests(self.args())
        previous_latest = self.root / "previous-latest.json"
        previous_preview = self.root / "previous-preview.json"
        previous_latest.write_text(json.dumps(first), encoding="utf-8")
        previous_preview.write_text(json.dumps(first_preview), encoding="utf-8")

        latest, preview = herdl_release.generate_manifests(
            self.args(
                build_id="123456abcdef",
                commit="123456abcdef7890123456789012345678901234",
                previous_latest=previous_latest,
                previous_preview=previous_preview,
            )
        )

        self.assertIn("0.9.0-vsh.abcdef123456", latest["releases"])
        self.assertIn("0.9.0-vsh.123456abcdef", latest["releases"])
        self.assertIn("abcdef123456", preview["builds"])
        self.assertIn("123456abcdef", preview["builds"])

    def test_identity_must_match_commit_prefix(self):
        with self.assertRaisesRegex(ValueError, "12-character prefix"):
            herdl_release.generate_manifests(self.args(build_id="wrong"))


if __name__ == "__main__":
    unittest.main()
