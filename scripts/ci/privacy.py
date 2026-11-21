"""privacy: run the privacy gate over the tree that is, or would be, public.

Why a copy: the gate reads a folder, so it is given exactly the files that ship, in their shape, and
nothing else. In the private repository that is every tracked file `.publicignore` does not list (the
selection scripts/publish-source.ps1 makes), with the words in `.private-words` added to
KIKU_PRIVATE_WORDS. In the public repository there is no `.publicignore`: every tracked file, with
whatever KIKU_PRIVATE_WORDS the environment has (empty for outside pull requests).
"""
import base64
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PUBLIC_IGNORE = ROOT / ".publicignore"
PRIVATE_WORDS = ROOT / ".private-words"
GATE = Path(__file__).resolve().parents[1] / "privacy-gate.py"


def git_files(*args):
    """Paths printed by `git ls-files <args>`, relative to the repository root."""
    out = subprocess.run(["git", "ls-files", "-z", *args], cwd=ROOT, check=True, capture_output=True).stdout
    return [path for path in out.decode("utf-8").split("\0") if path]


def public_files():
    """The tracked files that are, or would be, published."""
    tracked = git_files()
    if not PUBLIC_IGNORE.is_file():
        return tracked
    private = set(git_files("--cached", "--ignored", "--exclude-from", ".publicignore"))
    return [path for path in tracked if path not in private]


def words():
    """KIKU_PRIVATE_WORDS plus, in the private repository, the words `.private-words` holds."""
    found = [os.environ.get("KIKU_PRIVATE_WORDS", "")]
    if PUBLIC_IGNORE.is_file():
        found.append(base64.b64decode(PRIVATE_WORDS.read_text(encoding="ascii")).decode("utf-8"))
    return ",".join(word for word in found if word)


def copy_public(folder):
    """Copy the public files into `folder`, keeping their paths; the paths copied."""
    copied = []
    for path in public_files():
        source = ROOT / path
        if source.is_file():
            (Path(folder) / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, Path(folder) / path)
            copied.append(path)
    return copied


def needs_words(env=os.environ):
    """Whether this run must have private words: CI on anything but a pull request, which gets no secrets from a fork."""
    return env.get("GITHUB_ACTIONS") == "true" and env.get("GITHUB_EVENT_NAME") != "pull_request"


def gate(*paths):
    """Run the privacy gate over files and folders with the private words added; print its report, return its exit code.

    Every gate in CI goes through here. With no words at all it fails where words are expected (a missing or
    unreadable KIKU_PRIVATE_WORDS secret in the public repository), rather than pass on patterns alone.
    """
    private_words = words()
    if needs_words() and not any(word.strip() for word in private_words.split(",")):
        print("privacy: no private words (KIKU_PRIVATE_WORDS is empty and there is no .private-words): refusing to gate with patterns only", file=sys.stderr)
        return 1
    env = {**os.environ, "KIKU_PRIVATE_WORDS": private_words}
    done = subprocess.run([sys.executable, str(GATE), *map(str, paths)], env=env, capture_output=True, text=True, errors="replace")
    print(done.stdout, end="")
    print(done.stderr, end="", file=sys.stderr)
    return done.returncode


def main():
    """Copy the public tree to a temp folder, gate it, and return the gate's exit code."""
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kiku-public-", ignore_cleanup_errors=True) as folder:
        copied = len(copy_public(folder))
        code = gate(folder)
    print(f"privacy: {copied} files, {'ok' if code == 0 else 'FAILED'} in {time.monotonic() - started:.1f} s")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
