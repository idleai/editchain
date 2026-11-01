"""Check automatic Linux publication and additive manual platform publication."""

import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("publish_native", Path(__file__).with_name("publish-native.py"))
NATIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(NATIVE)


class NativePublicationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.tag = "fixture-v1.0.0"

    def bundle(self, platform):
        archive = self.directory / f"{self.tag}-{platform}.tar.gz"
        archive.write_bytes(platform.encode())
        checksum = archive.with_name(archive.name + ".sha256")
        checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
        return [archive.name, checksum.name]

    def publish(self, *, draft=True, existing=(), all_platforms=False):
        release = {"isDraft": draft, "assets": [{"name": name} for name in existing]}
        with patch.object(NATIVE.subprocess, "check_output", return_value=json.dumps(release)), \
                patch.object(NATIVE.subprocess, "run") as run:
            NATIVE.publish("idleai/fixture", self.tag, self.directory, all_platforms=all_platforms)
        return [call.args[0] for call in run.call_args_list]

    def test_automatic_release_completes_with_linux_only(self):
        self.bundle("linux-x64")
        calls = self.publish()
        self.assertEqual(len(calls), 2)
        self.assertIn("upload", calls[0])
        self.assertIn("--draft=false", calls[1])

    def test_manual_build_attaches_other_platforms_without_overwriting_linux(self):
        existing = self.bundle("linux-x64")
        for platform in NATIVE.PLATFORMS[1:]:
            self.bundle(platform)
        calls = self.publish(draft=False, existing=existing, all_platforms=True)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0:3], ["gh", "release", "upload"])
        self.assertNotIn("--clobber", calls[0])
        self.assertFalse(any("linux-x64" in argument for argument in calls[0]))
        for platform in NATIVE.PLATFORMS[1:]:
            self.assertTrue(any(platform in argument for argument in calls[0]))

    def test_manual_retry_leaves_complete_public_release_unchanged(self):
        existing = [name for platform in NATIVE.PLATFORMS for name in self.bundle(platform)]
        self.assertEqual(self.publish(draft=False, existing=existing, all_platforms=True), [])

    def test_automatic_job_cannot_change_a_public_release(self):
        self.bundle("linux-x64")
        with self.assertRaisesRegex(ValueError, "draft release"):
            self.publish(draft=False)

    def test_incomplete_manual_output_changes_nothing(self):
        self.bundle("linux-x64")
        with patch.object(NATIVE.subprocess, "check_output", return_value=json.dumps({"isDraft": True, "assets": []})), \
                patch.object(NATIVE.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "missing native bundle"):
                NATIVE.publish("idleai/fixture", self.tag, self.directory, all_platforms=True)
            run.assert_not_called()

    def test_partial_existing_assets_require_repair_before_publication(self):
        names = self.bundle("linux-x64")
        with self.assertRaisesRegex(ValueError, "incomplete existing"):
            self.publish(existing=names[:1])

    def test_invalid_archive_checksum_is_rejected(self):
        names = self.bundle("linux-x64")
        (self.directory / names[0]).write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "invalid native bundle checksum"):
            self.publish()

    def test_platform_vsix_files_can_be_attached_to_a_public_release(self):
        for platform in NATIVE.PLATFORMS:
            names = self.bundle(platform)
            archive = self.directory / names[0]
            vsix = archive.with_name(f"{self.tag}-{platform}.vsix")
            archive.rename(vsix)
            (self.directory / names[1]).rename(vsix.with_name(vsix.name + ".sha256"))
            checksum = vsix.with_name(vsix.name + ".sha256")
            checksum.write_text(checksum.read_text().replace(names[0], vsix.name))
        with patch.object(NATIVE.subprocess, "check_output", return_value=json.dumps({"isDraft": False, "assets": []})), \
                patch.object(NATIVE.subprocess, "run") as run:
            NATIVE.publish("idleai/fixture", self.tag, self.directory, all_platforms=True, extension="vsix")
        self.assertEqual(run.call_count, 1)
        self.assertEqual(sum(argument.endswith(".vsix") for argument in run.call_args.args[0]), 4)


if __name__ == "__main__":
    unittest.main()
