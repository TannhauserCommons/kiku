"""privacy: run the privacy gate over the tree that is public: every tracked file.

There is one repository and it is public, so what is committed is published. The files that must
stay private are gitignored (the `BEGIN private` block of `.gitignore`) and never committed; this
module refuses them if they are force-added anyway, and gates every tracked file for the private
words: `KIKU_PRIVATE_WORDS` (a CI secret) plus the words `.private-words` holds on a machine that
has it. The git hooks (`.githooks/`, `scripts/ci/hooks.py`) reuse it before a commit and a push.
"""
import base64
import binascii
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GITIGNORE = ROOT / ".gitignore"
PRIVATE_WORDS = ROOT / ".private-words"
GATE = Path(__file__).resolve().parents[1] / "privacy-gate.py"
BEGIN, END = "# BEGIN private", "# END private"


def load_gate():
    """scripts/privacy-gate.py as a module (its name has a hyphen), for its patterns and its word rules."""
    spec = importlib.util.spec_from_file_location("privacy_gate", GATE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(*args, root=ROOT, input=None):
    """Bytes git prints for `args` in `root`; a non-zero exit stops with git's message."""
    done = subprocess.run(["git", *args], cwd=root, input=input, capture_output=True)
    if done.returncode != 0:
        raise SystemExit(f"privacy: git {' '.join(args)}: {done.stderr.decode('utf-8', 'replace').strip()}")
    return done.stdout


def git_files(*args):
    """Paths printed by `git ls-files <args>`, relative to the repository root."""
    return [path for path in git("ls-files", "-z", *args).decode("utf-8").split("\0") if path]


def private_paths(gitignore=None):
    """The private paths: the `BEGIN private` block of `.gitignore`, as written (`/AGENTS.md`, `/skills/`)."""
    lines = (gitignore or GITIGNORE).read_text(encoding="utf-8").splitlines()
    starts = [i for i, line in enumerate(lines) if line.startswith(BEGIN)]
    if not starts or END not in lines[starts[0]:]:
        raise SystemExit(f"privacy: .gitignore has no '{BEGIN}' ... '{END}' block listing the private paths")
    block = lines[starts[0] + 1 : lines.index(END, starts[0])]
    return [line.strip() for line in block if line.strip() and not line.startswith("#")]


def is_private(path, entries):
    """Whether the repository path matches an entry, by gitignore's rules for the forms the block uses.

    A name alone (`AGENTS.md`, `skills/`) matches that name as any part of the path, at any depth; an entry with a
    slash before its end (`/docs/RELEASING.md`) is from the root; a trailing slash means a folder, and everything in it.
    """
    parts = path.replace("\\", "/").lstrip("/").split("/")
    for entry in entries:
        folder, name = entry.endswith("/"), entry.strip("/")
        if "/" in name:
            wanted = name.split("/")
            if parts[: len(wanted)] == wanted and (len(parts) > len(wanted) or not folder):
                return True
        elif name in parts[:-1] or (not folder and parts[-1] == name):
            return True
    return False


def private_in(paths, entries=None):
    """The paths that are private."""
    entries = private_paths() if entries is None else entries
    return [path for path in paths if is_private(path, entries)]


def words_file():
    """`.private-words` here, else in the main checkout when this is a linked worktree, else None."""
    if PRIVATE_WORDS.is_file():
        return PRIVATE_WORDS
    try:
        common = git("rev-parse", "--path-format=absolute", "--git-common-dir").decode().strip()
    except SystemExit:
        return None
    other = Path(common).parent / ".private-words"
    return other if other.is_file() else None


def words():
    """KIKU_PRIVATE_WORDS plus the words `.private-words` holds (base64 of comma-separated words), never printed.

    A `.private-words` that cannot be decoded stops the run: gating without it would pass a leak.
    """
    found = [os.environ.get("KIKU_PRIVATE_WORDS", "")]
    path = words_file()
    if path:
        try:
            found.append(base64.b64decode("".join(path.read_text(encoding="ascii").split()), validate=True).decode("utf-8"))
        except (binascii.Error, UnicodeError, ValueError):
            raise SystemExit("privacy: .private-words is not base64 text (its contents are not shown); fix or remove it") from None
    return ",".join(word for word in found if word)


def has_words(text=None):
    """Whether `text` (default: `words()`) holds a word long enough for the gate to use."""
    shortest = load_gate().SHORTEST_WORD
    return any(len(word.strip()) >= shortest for word in (words() if text is None else text).split(","))


def copy_public(folder):
    """Copy the tracked files into `folder`, keeping their paths; the paths copied."""
    copied = []
    for path in git_files():
        source = ROOT / path
        if source.is_file():
            (Path(folder) / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, Path(folder) / path)
            copied.append(path)
    return copied


def needs_words(env=os.environ):
    """Whether this run must have private words: any CI run but a pull request from a fork, which gets no secrets.

    The workflow sets KIKU_FORK_PR=true for those; anything else in CI, a push or a same-repository pull request
    included, fails without the words.
    """
    return env.get("GITHUB_ACTIONS") == "true" and env.get("KIKU_FORK_PR") != "true"


def gate(*paths):
    """Run the privacy gate over files and folders with the private words added; print its report, return its exit code.

    Every gate in CI goes through here. With no words at all it fails where words are expected (a missing or
    unreadable KIKU_PRIVATE_WORDS secret), rather than pass on patterns alone.
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
    """Refuse tracked private paths, copy the tracked files to a temp folder, gate it, and return the exit code."""
    started = time.monotonic()
    leaked = private_in(git_files())
    for path in leaked:
        print(f"privacy: {path} is private (.gitignore) but tracked: git rm --cached it", file=sys.stderr)
    with tempfile.TemporaryDirectory(prefix="kiku-public-", ignore_cleanup_errors=True) as folder:
        copied = len(copy_public(folder))
        code = gate(folder)
    code = code or int(bool(leaked))
    print(f"privacy: {copied} files, {'ok' if code == 0 else 'FAILED'} in {time.monotonic() - started:.1f} s")
    return code


if __name__ == "__main__":
    raise SystemExit(main())
