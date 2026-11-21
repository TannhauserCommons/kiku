"""land <sha>: move the public repository's main to the released commit.

The rolling commit on next has main's tip as its parent, so this is a fast-forward. Safe to run again: a main
that already points at the commit is a rerun. DRY_RUN=1 prints what it would push.
"""
import os
import re
import subprocess
import sys

from common import ROOT

REF = "refs/heads/main"


def decide(main, sha, main_is_ancestor):
    """`done` when main is at sha, `push` when sha can fast-forward it (or main does not exist), else `diverged`."""
    if main == sha:
        return "done"
    return "push" if main is None or main_is_ancestor else "diverged"


def git(*args, check=True):
    """Run git in the repository and return the completed process."""
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True, check=check)


def remote_main():
    """The commit origin's main points at, or None when it has none yet."""
    out = git("ls-remote", "origin", REF).stdout.split()
    return out[0] if out else None


def is_ancestor(old, new):
    """Whether commit `old` is an ancestor of `new`; fails on any other git error (a shallow clone, say)."""
    code = git("merge-base", "--is-ancestor", old, new, check=False).returncode
    if code not in (0, 1):
        sys.exit(f"land: git could not compare {old[:7]} with {new[:7]}")
    return code == 0


def main(argv):
    """Land the commit named by argv[0]; return the exit code."""
    if len(argv) != 1 or not re.fullmatch(r"[0-9a-f]{40}", argv[0]):
        print("usage: land.py <40-hex commit>", file=sys.stderr)
        return 2
    sha, current = argv[0], remote_main()
    if current:
        git("fetch", "origin", "main")
    verdict = decide(current, sha, current is not None and is_ancestor(current, sha))
    if verdict == "done":
        print(f"land: main is at {sha[:7]} already")
        return 0
    if verdict == "diverged":
        print(f"land: main has moved away from the candidate's parent: it is at {current[:7]}, which is not an ancestor of {sha[:7]}")
        return 1
    if os.environ.get("DRY_RUN") == "1":
        print(f"dry run: would push {sha[:7]} to main (now {current[:7] if current else 'none'})")
        return 0
    pushed = git("push", "origin", f"{sha}:{REF}", check=False)
    if pushed.returncode:
        print(f"land: the push was refused (main has moved away from the candidate's parent?)\n{pushed.stderr.strip()}")
        return 1
    print(f"land: main is now {sha[:7]}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
