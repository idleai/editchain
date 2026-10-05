"""Exercise release retries and concurrent changes using local Git repositories."""

import contextlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("release_automation", Path(__file__).with_name("release-automation.py"))
AUTOMATION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUTOMATION)


class ReleasePreparationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.run_git(self.root, "init", "--bare", str(self.remote))
        self.run_git(self.root, "clone", str(self.remote), str(self.checkout))
        self.run_git(self.checkout, "checkout", "-b", "main")
        self.run_git(self.checkout, "config", "user.name", "Fixture")
        self.run_git(self.checkout, "config", "user.email", "fixture@example.test")
        (self.checkout / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
        (self.checkout / "source.rs").write_text("original\n")
        self.commit("initial")
        self.run_git(self.checkout, "tag", "fixture-v0.1.0")
        self.run_git(self.checkout, "push", "origin", "main", "--tags")
        self.source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.release_command("from pathlib import Path\np = Path('Cargo.toml')\np.write_text(p.read_text().replace('0.1.0', '0.1.1'))\n")

    @staticmethod
    def run_git(directory, *arguments):
        return subprocess.check_output(["git", *arguments], cwd=directory, text=True, stderr=subprocess.DEVNULL).strip()

    def commit(self, message):
        self.run_git(self.checkout, "add", ".")
        self.run_git(self.checkout, "commit", "-m", message)

    def release_command(self, code):
        executable = self.bin / "release-plz"
        executable.write_text("#!/usr/bin/env python3\n" + code)
        executable.chmod(0o755)

    def prepare(self, source=None, version="0.1.1"):
        with contextlib.chdir(self.checkout), patch.dict(os.environ, {"PATH": f"{self.bin}:{os.environ['PATH']}"}), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": version}]), \
                patch.object(AUTOMATION, "output") as output:
            AUTOMATION.prepare(source or self.source)
            return output.call_args.args[1]

    def test_version_commit_is_pushed_once_and_retry_reuses_it(self):
        revision = self.prepare()
        self.assertNotEqual(revision, self.source)
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), revision)
        self.assertEqual(self.run_git(self.checkout, "rev-parse", "HEAD^"), self.source)
        self.assertEqual(self.prepare(), revision)
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), revision)

    def test_superseded_source_cannot_publish(self):
        (self.checkout / "source.rs").write_text("a newer change\n")
        self.commit("newer main")
        self.run_git(self.checkout, "push", "origin", "main")
        self.assertEqual(self.prepare(), "")
        self.assertEqual((self.checkout / "Cargo.toml").read_text().count("0.1.0"), 1)

    def test_non_metadata_change_is_rejected_before_push(self):
        self.release_command("from pathlib import Path\nPath('source.rs').write_text('unexpected')\n")
        with self.assertRaisesRegex(ValueError, "outside release metadata"):
            self.prepare()
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), self.source)

    def test_parallel_push_cannot_be_overwritten(self):
        other = self.root / "other"
        self.run_git(self.root, "clone", "-b", "main", str(self.remote), str(other))
        self.run_git(other, "config", "user.name", "Fixture")
        self.run_git(other, "config", "user.email", "fixture@example.test")
        (other / "source.rs").write_text("concurrent main\n")
        self.run_git(other, "commit", "-am", "concurrent change")
        concurrent = self.run_git(other, "rev-parse", "HEAD")
        self.release_command(f"import subprocess\nfrom pathlib import Path\nsubprocess.run(['git', '-C', {str(other)!r}, 'push', 'origin', 'main'], check=True)\np = Path('Cargo.toml')\np.write_text(p.read_text().replace('0.1.0', '0.1.1'))\n")
        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare()
        self.assertEqual(self.run_git(self.remote, "rev-parse", "main"), concurrent)

    def test_docs_only_commit_does_not_need_another_release(self):
        (self.checkout / "notes.txt").write_text("documentation\n")
        self.commit("docs")
        self.run_git(self.checkout, "push", "origin", "main")
        source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.release_command("pass\n")
        self.assertEqual(self.prepare(source, version="0.1.0"), "")

    def test_manual_version_bump_can_publish_without_another_commit(self):
        (self.checkout / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.2.0"\n')
        self.commit("breaking release")
        self.run_git(self.checkout, "push", "origin", "main")
        source = self.run_git(self.checkout, "rev-parse", "HEAD")
        self.release_command("pass\n")
        self.assertEqual(self.prepare(source, version="0.2.0"), source)

    def test_artifact_retry_selects_draft_and_leaves_public_release_unchanged(self):
        self.prepare()
        self.run_git(self.checkout, "tag", "fixture-v0.1.1")
        original = AUTOMATION.command
        for draft in (True, False):
            def command(*arguments):
                if arguments[:3] == ("gh", "release", "view"):
                    return json.dumps({"isDraft": draft})
                return original(*arguments)
            with contextlib.chdir(self.checkout), \
                    patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.1"}]), \
                    patch.object(AUTOMATION, "command", side_effect=command), patch.object(AUTOMATION, "output") as output:
                AUTOMATION.artifact("fixture")
                output.assert_called_once_with("tag", "fixture-v0.1.1" if draft else "")

    def test_missing_artifact_tag_fails_instead_of_reporting_publication_success(self):
        with contextlib.chdir(self.checkout), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.1"}]):
            with self.assertRaisesRegex(ValueError, "did not create the artifact tag"):
                AUTOMATION.artifact("fixture")

    def test_unchanged_artifact_from_an_older_commit_is_not_rebuilt(self):
        (self.checkout / "source.rs").write_text("another package changed\n")
        self.commit("another package")
        with contextlib.chdir(self.checkout), \
                patch.object(AUTOMATION, "packages", return_value=[{"name": "fixture", "version": "0.1.0"}]), \
                patch.object(AUTOMATION, "output") as output:
            AUTOMATION.artifact("fixture")
            output.assert_called_once_with("tag", "")


if __name__ == "__main__":
    unittest.main()
