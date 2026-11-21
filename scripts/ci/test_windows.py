"""Tests for the pure parts of the Windows build and smoke. Run: python -m unittest discover -s scripts/ci"""
import hashlib
import re
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

import build_windows
import common
import smoke_windows


class BuildTest(unittest.TestCase):
    def test_update_key_is_read_only_when_releases_is_set(self):
        self.assertIsNone(build_windows.update_key({}))
        key = build_windows.update_key({"KIKU_RELEASES": "https://x/releases"})
        self.assertRegex(key, r"^[0-9a-f]{64}$")

    def test_a_bad_key_file_fails(self):
        with tempfile.TemporaryDirectory() as folder:
            bad = Path(folder) / "update-key.txt"
            bad.write_text("# not a key\n")
            with mock.patch.object(build_windows, "UPDATE_KEY", bad), self.assertRaises(SystemExit):
                build_windows.update_key({"KIKU_RELEASES": "x"})

    def test_iscc_is_looked_for_in_both_program_files(self):
        env = {"ProgramFiles(x86)": r"C:\X86", "ProgramFiles": r"C:\PF"}
        paths = build_windows.iscc_candidates(env)
        self.assertEqual([p.replace("\\", "/") for p in paths], ["C:/X86/Inno Setup 6/ISCC.exe", "C:/PF/Inno Setup 6/ISCC.exe"])
        self.assertIsNone(build_windows.find_iscc(env))

    def test_inno_setup_is_pinned_by_version_and_hash(self):
        self.assertRegex(build_windows.INNO_URL, r"/is-\d+_\d+_\d+/innosetup-\d+\.\d+\.\d+\.exe$")
        self.assertRegex(build_windows.INNO_SHA256, r"^[0-9a-f]{64}$")

    def test_a_download_is_kept_only_when_it_hashes_to_the_pin(self):
        with tempfile.TemporaryDirectory(ignore_cleanup_errors=True) as folder:
            source, target = Path(folder, "source.exe"), Path(folder, "target.exe")
            source.write_bytes(b"installer")
            build_windows.download_verified(source.as_uri(), hashlib.sha256(b"installer").hexdigest(), target)
            self.assertEqual(target.read_bytes(), b"installer")
            target.unlink()
            with self.assertRaises(SystemExit) as refused:
                build_windows.download_verified(source.as_uri(), "0" * 64, target)
            self.assertIn("not the pinned", str(refused.exception))
            self.assertFalse(target.exists())

    def test_a_missing_iscc_after_installing_is_a_clear_failure(self):
        with mock.patch.object(build_windows, "find_iscc", return_value=None), mock.patch.object(build_windows, "install_inno"):
            with self.assertRaises(SystemExit) as refused:
                build_windows.iscc()
        self.assertIn("ISCC.exe is not in", str(refused.exception))


class RunLimitedTest(unittest.TestCase):
    ORPHAN = "import subprocess, sys; subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(8)'])"

    def test_a_child_left_behind_does_not_hold_the_call_up(self):
        started = time.monotonic()
        code, output = common.run_limited([sys.executable, "-c", self.ORPHAN], 30, capture=False)
        self.assertEqual((code, output), (0, ""))
        self.assertLess(time.monotonic() - started, 6)


class SmokeTest(unittest.TestCase):
    def test_differing_names_missing_and_changed_files(self):
        with tempfile.TemporaryDirectory() as folder:
            staged, installed = Path(folder, "staged"), Path(folder, "installed")
            for root in (staged, installed):
                (root / "models").mkdir(parents=True)
            for name in ("kiku.exe", "models/silero_vad.onnx", "kiku-engine.exe"):
                (staged / name).write_bytes(b"same")
            (installed / "kiku.exe").write_bytes(b"same")
            (installed / "models/silero_vad.onnx").write_bytes(b"other")
            (installed / "unins000.exe").write_bytes(b"extra")
            found = [re.sub(r"\\", "/", name) for name in smoke_windows.differing(staged, installed)]
            self.assertEqual(sorted(found), ["kiku-engine.exe", "models/silero_vad.onnx"])


if __name__ == "__main__":
    unittest.main()
