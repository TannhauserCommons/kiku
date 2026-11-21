"""Tests for land.py against real git repositories. Run: python -m unittest discover -s scripts/ci"""
import io
import subprocess
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import land


class DecideTest(unittest.TestCase):
    def test_verdicts(self):
        self.assertEqual(land.decide("a", "a", False), "done")
        self.assertEqual(land.decide("a", "b", True), "push")
        self.assertEqual(land.decide(None, "b", False), "push")
        self.assertEqual(land.decide("a", "b", False), "diverged")


def git(folder, *args):
    return subprocess.run(["git", "-C", str(folder), "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
                          check=True, capture_output=True, text=True).stdout.strip()


class LandTest(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        git(root, "init", "-q", "--bare", "origin.git")
        git(root, "clone", "-q", "origin.git", "work")
        self.work = root / "work"
        git(self.work, "checkout", "-q", "-b", "next")
        self.base = self.commit("base")
        git(self.work, "push", "-q", "origin", "next:main")
        self.rolling = self.commit("rolling")
        patch = mock.patch.object(land, "ROOT", self.work)
        patch.start()
        self.addCleanup(patch.stop)
        self.origin_main = lambda: git(root / "origin.git", "rev-parse", "refs/heads/main")

    def commit(self, name):
        (self.work / name).write_text(name)
        git(self.work, "add", name)
        git(self.work, "commit", "-q", "-m", name)
        return git(self.work, "rev-parse", "HEAD")

    def land(self, sha):
        with redirect_stdout(io.StringIO()) as out:
            code = land.main([sha])
        return code, out.getvalue()

    def test_fast_forwards_main_and_a_rerun_finds_it_there(self):
        self.assertEqual(self.land(self.rolling)[0], 0)
        self.assertEqual(self.origin_main(), self.rolling)
        code, out = self.land(self.rolling)
        self.assertEqual(code, 0)
        self.assertIn("already", out)

    def test_main_that_moved_away_fails(self):
        git(self.work, "checkout", "-q", "-b", "other", self.base)
        other = self.commit("other")
        git(self.work, "push", "-q", "origin", "other:main")
        code, out = self.land(self.rolling)
        self.assertEqual(code, 1)
        self.assertIn("main has moved away from the candidate's parent", out)
        self.assertEqual(self.origin_main(), other)

    def test_dry_run_pushes_nothing(self):
        with mock.patch.dict("os.environ", {"DRY_RUN": "1"}):
            code, out = self.land(self.rolling)
        self.assertEqual(code, 0)
        self.assertIn("would push", out)
        self.assertEqual(self.origin_main(), self.base)

    def test_bad_argument(self):
        with redirect_stdout(io.StringIO()), mock.patch("sys.stderr", io.StringIO()):
            self.assertEqual(land.main(["main"]), 2)


if __name__ == "__main__":
    unittest.main()
