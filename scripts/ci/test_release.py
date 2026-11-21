"""Tests for the pure parts of the build, smoke and publish scripts. Run: python -m unittest discover -s scripts/ci"""
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import common
import image
import names
import publish


class NamesTest(unittest.TestCase):
    def test_file_names(self):
        self.assertEqual(names.installer("1.2.3", "arm64"), "Kiku-1.2.3-arm64-setup.exe")
        self.assertEqual(names.tarball("1.2.3"), "kiku-engine-1.2.3-linux-x64.tar.gz")

    def test_image_is_lower_case(self):
        self.assertEqual(names.image("TannhauserCommons/kiku"), "ghcr.io/tannhausercommons/kiku-engine")

    def test_downloads_say_which_file_is_for_whom(self):
        text = names.downloads("Owner/kiku", "1.2.3")
        base = "https://github.com/Owner/kiku/releases/download/v1.2.3"
        self.assertTrue(text.startswith("### Downloads\n\n- Windows 10 and 11 on x64"))
        self.assertIn(f"[Kiku-1.2.3-x64-setup.exe]({base}/Kiku-1.2.3-x64-setup.exe)", text)
        self.assertIn(f"- Windows 11 on ARM, such as Snapdragon laptops: [Kiku-1.2.3-arm64-setup.exe]({base}/Kiku-1.2.3-arm64-setup.exe)", text)
        self.assertIn(f"[kiku-engine-1.2.3-linux-x64.tar.gz]({base}/kiku-engine-1.2.3-linux-x64.tar.gz), or the Docker image `ghcr.io/owner/kiku-engine`", text)
        self.assertTrue(text.endswith("\n"))


class CommonTest(unittest.TestCase):
    def test_lists_model_wants_the_id_at_a_line_start(self):
        out = "moonshine-tiny  Moonshine Tiny  cpu: not installed\nparakeet-unified  X\n"
        self.assertTrue(common.lists_model(out, "moonshine-tiny"))
        self.assertFalse(common.lists_model(out, "moonshine"))
        self.assertFalse(common.lists_model("error: moonshine-tiny missing", "moonshine-tiny"))

    def test_windows_arch_is_windows_not_pythons(self):
        self.assertEqual(common.windows_arch({"PROCESSOR_ARCHITECTURE": "AMD64"}), "x64")
        self.assertEqual(common.windows_arch({"PROCESSOR_ARCHITECTURE": "ARM64"}), "arm64")
        emulated = {"PROCESSOR_ARCHITECTURE": "AMD64", "PROCESSOR_ARCHITEW6432": "ARM64"}
        self.assertEqual(common.windows_arch(emulated), "arm64")
        with self.assertRaises(SystemExit):
            common.windows_arch({})

    def test_first_model_id_is_in_the_catalog(self):
        self.assertRegex(common.first_model_id(), r"^[a-z0-9-]+$")

    def test_run_limited_returns_output_and_stops_a_hang(self):
        code, output = common.run_limited([sys.executable, "-c", "print('hi'); raise SystemExit(3)"], 30)
        self.assertEqual((code, output.strip()), (3, "hi"))
        with self.assertRaises(SystemExit) as stopped:
            common.run_limited([sys.executable, "-c", "import time; time.sleep(60)"], 1)
        self.assertIn("did not finish in 1 s", str(stopped.exception))


class PublishTest(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.dist = Path(folder.name)
        patches = [mock.patch.object(publish, "DIST", self.dist), mock.patch.object(publish, "version", lambda: "1.2.3"),
                   mock.patch.object(publish, "run", self.fake_run), mock.patch.object(publish, "key_matches", lambda: True),
                   mock.patch.object(publish.privacy, "gate", lambda *paths: 0), mock.patch.object(publish, "UPDATE_KEY", self.key_file()), mock.patch.dict("os.environ", {"KIKU_RELEASES": "https://x/releases"})]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def key_file(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        path = Path(folder.name, "update-key.txt")
        path.write_text("ab" * 32 + "\n")
        return path

    def fake_run(self, command, **kwargs):
        if "manifest" in command:
            setup = Path(command[4])
            (self.dist / ("latest.json" if "-x64-" in setup.name else "latest-arm64.json")).write_text(setup.name)

    def make(self, *files):
        for name in files:
            (self.dist / name).write_bytes(name.encode())

    def test_missing_files_fail(self):
        self.make("Kiku-1.2.3-x64-setup.exe")
        with redirect_stdout(io.StringIO()) as out:
            self.assertEqual(publish.main(), 1)
        self.assertIn("Kiku-1.2.3-arm64-setup.exe, kiku-engine-1.2.3-linux-x64.tar.gz", out.getvalue())

    def test_twice_gives_the_same_files(self):
        self.make(*publish.needed("1.2.3"))
        with redirect_stdout(io.StringIO()):
            self.assertEqual(publish.main(), 0)
            first = {p.name: p.read_bytes() for p in self.dist.iterdir()}
            self.assertEqual(publish.main(), 0)
            second = {p.name: p.read_bytes() for p in self.dist.iterdir()}
        self.assertEqual(first, second)
        self.assertEqual(set(first), {*publish.needed("1.2.3"), "Kiku-setup.exe", "Kiku-arm64-setup.exe", "kiku-engine-linux-x64.tar.gz", "latest.json", "latest-arm64.json"})
        self.assertEqual(first["Kiku-arm64-setup.exe"], first["Kiku-1.2.3-arm64-setup.exe"])


class ImageTest(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        Path(folder.name, "kiku-engine-1.2.3-linux-x64.tar.gz").write_bytes(b"t")
        self.commands, self.stdin = [], []
        env = {"GITHUB_REPOSITORY": "Owner/kiku", "GITHUB_ACTOR": "bot", "GH_TOKEN": "secret-token"}
        patches = [mock.patch.object(image, "CONTEXT", Path(folder.name)), mock.patch.object(image, "version", lambda: "1.2.3"),
                   mock.patch.object(image, "run", lambda command, **kwargs: self.commands.append(command)),
                   mock.patch.object(image.subprocess, "run", self.login), mock.patch.dict("os.environ", env)]
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)

    def login(self, command, input, text):
        self.commands.append(command)
        self.stdin.append(input)
        return mock.Mock(returncode=0)

    def pushed(self):
        return [c[2] for c in self.commands if c[:2] == ["docker", "push"]]

    def test_pushes_both_tags_and_logs_in_through_stdin(self):
        self.assertEqual(image.main(), 0)
        self.assertEqual(self.pushed(), ["ghcr.io/owner/kiku-engine:1.2.3", "ghcr.io/owner/kiku-engine:latest"])
        self.assertEqual(self.stdin, ["secret-token"])
        self.assertNotIn("secret-token", " ".join(" ".join(c) for c in self.commands))
        self.assertIn("org.opencontainers.image.source=https://github.com/Owner/kiku", self.commands[0])

    def test_dry_run_builds_but_does_not_push_or_log_in(self):
        with mock.patch.dict("os.environ", {"DRY_RUN": "1"}), redirect_stdout(io.StringIO()):
            self.assertEqual(image.main(), 0)
        self.assertEqual(self.pushed(), [])
        self.assertEqual(self.stdin, [])
        self.assertEqual(self.commands[0][:2], ["docker", "build"])


if __name__ == "__main__":
    unittest.main()
