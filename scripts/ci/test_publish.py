"""Tests of publish's key check and manifest verification with real keys and signatures (they run uv).

Run: python -m unittest discover -s scripts/ci
"""
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import names
import publish

UV = shutil.which("uv")


def uv(*args, key=None):
    """Run `uv run <args>` with KIKU_SIGNING_KEY set to `key` when given; return the completed process."""
    env = {**os.environ, **({"KIKU_SIGNING_KEY": key} if key else {})}
    return subprocess.run(["uv", "run", *map(str, args), ], env=env, capture_output=True, text=True)


@unittest.skipUnless(UV, "uv is not installed")
class KeyCheckTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.key = uv(publish.RELEASE, "keygen").stdout.strip()
        cls.public = uv(publish.RELEASE, "public-key", key=cls.key).stdout.strip()

    def setUp(self):
        folder = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        self.dist, self.key_file = root / "dist", root / "update-key.txt"
        self.dist.mkdir()
        for name in publish.needed("1.2.3"):
            (self.dist / name).write_bytes(name.encode())
        env = {"KIKU_SIGNING_KEY": self.key, "KIKU_RELEASES": "https://x/releases", "RELEASE_NOTES": "Faster."}
        patches = [mock.patch.object(publish, "DIST", self.dist), mock.patch.object(publish, "version", lambda: "1.2.3"),
                   mock.patch.object(publish, "UPDATE_KEY", self.key_file), mock.patch.object(publish.privacy, "gate", lambda *paths: 0),
                   mock.patch.dict("os.environ", env)]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def manifests(self):
        return sorted(p.name for p in self.dist.glob("latest*.json"))

    def test_a_key_that_is_not_the_committed_one_fails_before_any_manifest(self):
        self.key_file.write_text("cd" * 32 + "\n")
        with redirect_stdout(io.StringIO()) as out:
            self.assertEqual(publish.main(), 1)
        self.assertIn("KIKU_SIGNING_KEY does not match crates/core/update-key.txt; shipped copies would refuse this release's updates", out.getvalue())
        self.assertNotIn(self.key, out.getvalue())
        self.assertEqual(self.manifests(), [])

    def test_the_committed_key_signs_and_the_manifests_verify(self):
        self.key_file.write_text(self.public + "\n")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(publish.main(), 0)
        self.assertEqual(self.manifests(), ["latest-arm64.json", "latest.json"])
        self.assertEqual(names.installer("1.2.3", "x64"), json.loads((self.dist / "latest.json").read_text())["url"].rsplit("/", 1)[1])

    def test_a_tampered_manifest_fails_verification(self):
        self.key_file.write_text(self.public + "\n")
        with redirect_stdout(io.StringIO()):
            publish.main()
        path = self.dist / "latest.json"
        release = json.loads(path.read_text())
        release["notes"] = "Something else."
        path.write_text(json.dumps(release))
        done = uv(publish.VERIFY, self.public, path, self.dist / "latest-arm64.json")
        self.assertEqual(done.returncode, 1)
        self.assertIn("latest.json is not validly signed", done.stdout)
        self.assertIn("latest-arm64.json is signed by", done.stdout)

    def test_a_manifest_signed_by_another_key_fails_verification(self):
        self.key_file.write_text(self.public + "\n")
        with redirect_stdout(io.StringIO()):
            publish.main()
        other = uv(publish.RELEASE, "public-key", key=uv(publish.RELEASE, "keygen").stdout.strip()).stdout.strip()
        self.assertEqual(uv(publish.VERIFY, other, self.dist / "latest.json").returncode, 1)


if __name__ == "__main__":
    unittest.main()
