"""Tests for the git hooks (.githooks/, hooks.py), run as git runs them in real temporary repositories.
Run: python -m unittest discover -s scripts/ci

The private words here are made up. Every refusal is checked by what git did (no commit, nothing in the remote),
so a test fails when the hook is not there. History that a hook would have refused is made with git's plumbing,
which runs no hook, as it would be had the hooks not been installed.
"""
import base64
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import privacy

ROOT = Path(__file__).resolve().parents[2]
WORD = "zorblax"
# Built apart so that this file does not itself hold what the gate rejects.
HOME = "/" + "home/"
CREDIT = "Co-Authored" + "-By:"
COPIED = [".gitignore", ".githooks/pre-commit", ".githooks/pre-push", "scripts/privacy-gate.py", "scripts/ci/privacy.py", "scripts/ci/hooks.py"]


class Repo(unittest.TestCase):
    """A work repository with the hooks installed and a bare remote."""

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.addCleanup(self.folder.cleanup)
        self.work, self.remote = Path(self.folder.name, "work"), Path(self.folder.name, "remote.git")
        for name in COPIED:
            (self.work / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, self.work / name)
        self.git("init", "-q", "-b", "main", cwd=self.work)
        self.git("init", "-q", "--bare", self.remote, cwd=self.folder.name)
        self.git("remote", "add", "origin", str(self.remote))
        self.assertEqual(self.run_py("install").returncode, 0)
        done = self.commit("base", {"README.md": "hello\n", **{name: (self.work / name).read_text() for name in COPIED}})
        self.assertEqual(done.returncode, 0, done.stderr)

    def env(self, **extra):
        """No inherited words or git settings (the real ones must never reach a test); the made-up word by default."""
        env = {k: v for k, v in os.environ.items() if not k.startswith(("KIKU_", "GIT_")) and k not in ("USER", "USERNAME", "COMPUTERNAME", "GITHUB_ACTIONS")}
        env.update(KIKU_PRIVATE_WORDS=WORD, KIKU_PYTHON=sys.executable, GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
                   GIT_AUTHOR_NAME="Tester", GIT_AUTHOR_EMAIL="tester@example.com", GIT_COMMITTER_NAME="Tester", GIT_COMMITTER_EMAIL="tester@example.com")
        env.update(extra)
        return {k: v for k, v in env.items() if v is not None}

    def git(self, *args, cwd=None, env=None, check=True):
        done = subprocess.run(["git", *args], cwd=cwd or self.work, env=env or self.env(), capture_output=True, text=True, errors="replace")
        if check and done.returncode != 0:
            self.fail(f"git {' '.join(args)}: {done.stderr}")
        return done

    def run_py(self, *args):
        return subprocess.run([sys.executable, str(self.work / "scripts/ci/hooks.py"), *args], cwd=self.work, env=self.env(), capture_output=True, text=True)

    def head(self):
        return self.git("rev-parse", "-q", "--verify", "HEAD", check=False).stdout.strip()

    def stage(self, files):
        for name, text in files.items():
            (self.work / name).parent.mkdir(parents=True, exist_ok=True)
            (self.work / name).write_text(text, encoding="utf-8")
            self.git("add", "-f", name)  # -f: some tests stage private paths on purpose

    def commit(self, message, files, env=None):
        """`git commit`, hooks and all."""
        self.stage(files)
        return self.git("commit", "-q", "-m", message, env=env, check=False)

    def plant(self, message, files, env=None):
        """A commit made with git's plumbing, which runs no hook: history as it would be had the hooks not been there."""
        self.stage(files)
        tree = self.git("write-tree", env=env).stdout.strip()
        parent = self.head()
        made = self.git("commit-tree", tree, *(["-p", parent] if parent else []), "-m", message, env=env).stdout.strip()
        self.git("update-ref", "HEAD", made)

    def remote_refs(self):
        return self.git("for-each-ref", cwd=self.remote).stdout.strip()

    def refused(self, done, *needles):
        shown = done.stdout + done.stderr
        self.assertNotEqual(done.returncode, 0, shown)
        for needle in needles:
            self.assertIn(needle, shown)
        self.assertNotIn(WORD, shown.lower(), "the word must never be printed")


class PreCommitTest(Repo):
    def test_a_clean_commit_passes(self):
        before = self.head()
        self.assertEqual(self.commit("clean", {"a.txt": "fine\n"}).returncode, 0)
        self.assertNotEqual(self.head(), before)

    def test_a_staged_private_word_is_refused_in_any_case(self):
        before = self.head()
        self.refused(self.commit("leak", {"notes.txt": f"thanks, {WORD.upper()}\n"}), "privacy gate refused the staged files")
        self.assertEqual(self.head(), before)

    def test_the_staged_content_is_checked_not_the_working_file(self):
        before = self.head()
        self.stage({"x.txt": f"{WORD}\n"})
        (self.work / "x.txt").write_text("clean now\n")  # the working file is clean; the index is not
        self.refused(self.git("commit", "-q", "-m", "m", check=False))
        self.assertEqual(self.head(), before)

    def test_a_clean_index_passes_when_the_working_copy_has_the_word(self):
        self.stage({"x.txt": "clean\n"})
        (self.work / "x.txt").write_text(f"{WORD}\n")  # never staged
        self.assertEqual(self.git("commit", "-q", "-m", "m", check=False).returncode, 0)

    def test_a_force_added_private_file_is_refused(self):
        before = self.head()
        for name in ("AGENTS.md", "skills/a.md", ".private-words", "docs/RELEASING.md", "crates/x/CLAUDE.md"):
            done = self.commit("force", {name: "plain\n"})
            self.refused(done, f"{name} is private")
            self.assertEqual(self.head(), before)
            self.git("reset", "-q", "--hard", "HEAD")
            self.git("clean", "-fdqx")

    def test_without_words_it_refuses_and_the_override_checks_patterns_only(self):
        no_words = self.env(KIKU_PRIVATE_WORDS=None)
        before = self.head()
        self.refused(self.commit("m", {"a.txt": "fine\n"}, no_words), "no private words", "KIKU_PATTERNS_ONLY=1")
        self.assertEqual(self.head(), before)
        override = self.env(KIKU_PRIVATE_WORDS=None, KIKU_PATTERNS_ONLY="1")
        done = self.git("commit", "-q", "-m", "m", env=override, check=False)
        self.assertEqual(done.returncode, 0, done.stderr)
        # the override still gates the patterns
        self.refused(self.commit("m2", {"b.txt": f"see {HOME}someone/x\n"}, override), "a Unix home folder")

    def test_the_words_may_come_from_private_words(self):
        encoded = base64.b64encode(f"{WORD},quuxify".encode()).decode()
        (self.work / ".private-words").write_text(encoded[:8] + "\r\n" + encoded[8:] + "\r\n")  # wrapped and CRLF still decode
        env = self.env(KIKU_PRIVATE_WORDS=None)
        before = self.head()
        self.refused(self.commit("leak", {"n.txt": f"{WORD}\n"}, env), "privacy gate refused")
        self.assertEqual(self.head(), before)
        self.git("reset", "-q", "--hard", "HEAD")
        done = self.commit("ok", {"n.txt": "fine\n"}, env)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertNotIn(".private-words", self.git("ls-files").stdout)

    def test_a_broken_private_words_file_fails_closed(self):
        (self.work / ".private-words").write_text("not base64 !!\n")
        before = self.head()
        self.refused(self.commit("m", {"a.txt": "fine\n"}, self.env(KIKU_PRIVATE_WORDS=None)), "not base64")
        self.assertEqual(self.head(), before)


class PrePushTest(Repo):
    def push(self, *args, env=None):
        return self.git("push", "-q", "origin", *args, env=env, check=False)

    def test_a_clean_push_passes(self):
        self.commit("two", {"a.txt": "fine\n"})
        done = self.push("main")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("refs/heads/main", self.remote_refs())

    def test_a_word_in_an_older_commit_of_the_range_is_refused_though_the_tip_is_clean(self):
        self.plant("leak", {"notes.txt": f"{WORD}\n"})
        self.plant("tidy", {"notes.txt": "fine\n"})
        self.refused(self.push("main"), "privacy gate refused commit")
        self.assertEqual(self.remote_refs(), "")

    def test_only_commits_the_remote_lacks_are_checked(self):
        self.assertEqual(self.push("main").returncode, 0)
        self.plant("leak", {"notes.txt": f"{WORD}\n"})
        self.refused(self.push("main"), "privacy gate refused commit")
        self.git("reset", "-q", "--hard", "HEAD~1")
        self.commit("fine", {"c.txt": "fine\n"})
        self.assertEqual(self.push("main").returncode, 0)

    def test_a_new_branch_is_checked_against_what_the_remote_knows(self):
        self.assertEqual(self.push("main").returncode, 0)
        self.git("checkout", "-q", "-b", "topic")
        self.plant("leak", {"notes.txt": f"{WORD}\n"})
        self.refused(self.push("topic"), "privacy gate refused commit")
        self.assertNotIn("topic", self.remote_refs())

    def test_a_private_path_in_a_pushed_commit_is_refused_even_when_later_deleted(self):
        self.plant("add", {"AGENTS.md": "plain\n"})
        self.git("rm", "-q", "--cached", "AGENTS.md")
        self.assertEqual(self.git("commit", "-q", "-m", "drop", check=False).returncode, 0)
        self.refused(self.push("main"), "AGENTS.md is private")
        self.assertEqual(self.remote_refs(), "")

    def test_a_word_in_the_message_or_the_author_is_refused(self):
        self.plant(f"thanks {WORD}", {"a.txt": "fine\n"})
        self.refused(self.push("main"), "a private word in the author, committer or message")
        self.git("reset", "-q", "--hard", "HEAD~1")
        self.plant("fine", {"a.txt": "fine\n"}, self.env(GIT_AUTHOR_NAME=f"Mr {WORD}"))
        self.refused(self.push("main"), "a private word in the author")
        self.assertEqual(self.remote_refs(), "")

    def test_a_tool_credit_in_the_message_is_refused(self):
        self.plant(f"fine\n\n{CREDIT} Someone", {"a.txt": "fine\n"})
        self.refused(self.push("main"), "a tool credited as an author")
        self.assertEqual(self.remote_refs(), "")

    def test_without_words_it_refuses_and_the_override_lets_clean_commits_through(self):
        self.commit("two", {"a.txt": "fine\n"})
        self.refused(self.push("main", env=self.env(KIKU_PRIVATE_WORDS=None)), "no private words")
        self.assertEqual(self.remote_refs(), "")
        done = self.push("main", env=self.env(KIKU_PRIVATE_WORDS=None, KIKU_PATTERNS_ONLY="1"))
        self.assertEqual(done.returncode, 0, done.stderr)

    def test_deleting_a_branch_sends_no_commit_and_passes(self):
        self.assertEqual(self.push("main").returncode, 0)
        self.assertEqual(self.push("main:gone").returncode, 0)
        self.assertEqual(self.push("--delete", "gone").returncode, 0)


class InstallAndPathsTest(Repo):
    def test_install_is_idempotent(self):
        for _ in range(2):
            done = self.run_py("install")
            self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.git("config", "core.hooksPath").stdout.strip(), ".githooks")

    def test_the_private_block_lists_what_the_plan_names(self):
        entries = privacy.private_paths()
        for path in ("AGENTS.md", "CLAUDE.md", ".claude/x", "skills/a/b.md", "spikes/s", ".loop/l/p.md", "docs/RELEASING.md", ".private-words", "scripts/loop-gate.ps1", "scripts/setup-release.ps1"):
            self.assertTrue(privacy.is_private(path, entries), path)
        for path in ("crates/a/AGENTS.md", "crates/a/skills/x.md", "a/.claude/b", "a/b/.private-words"):
            self.assertTrue(privacy.is_private(path, entries), f"{path}: a name alone matches at any depth")
        for path in ("README.md", "docs/context/ci.md", "docs/RELEASING.md.bak", "a/docs/RELEASING.md", "a/scripts/loop-gate.ps1", "scripts/ci/hooks.py", "scripts/release.py", "AGENTS.mdx", ".github/workflows/check.yml"):
            self.assertFalse(privacy.is_private(path, entries), path)

    def test_a_tree_without_the_block_stops_the_check(self):
        (self.work / ".gitignore").write_text("/target\n")
        self.refused(self.commit("m", {"a.txt": "fine\n"}), "BEGIN private")


if __name__ == "__main__":
    unittest.main()
