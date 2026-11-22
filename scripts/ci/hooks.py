"""hooks: the git hooks that keep private files and words out of a public repository.

  python scripts/ci/hooks.py install      point git at .githooks/ (`mise run setup`); safe to run again
  python scripts/ci/hooks.py pre-commit   what .githooks/pre-commit runs: the staged tree
  python scripts/ci/hooks.py pre-push     what .githooks/pre-push runs: every commit being pushed

The repository is public: what is committed here is published when it is pushed, before any CI runs. So both hooks
refuse any path the `BEGIN private` block of `.gitignore` lists (a force-added file included) and run the privacy
gate (scripts/privacy-gate.py, through scripts/ci/privacy.py) over the commit's files with the private words in
KIKU_PRIVATE_WORDS and `.private-words`. The pre-push hook also looks at each commit's author, committer and
message. Without any private word the hooks refuse: a push the gate could not fully check is the leak to prevent.
Outside contributors, who do not have the words, set KIKU_PATTERNS_ONLY=1 to gate the patterns alone; CI still
requires the words and checks again.
"""
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

import privacy

OVERRIDE = "KIKU_PATTERNS_ONLY"
NO_WORDS = (
    "kiku hooks: no private words, so the privacy check cannot be complete.\n"
    "  Put them in the environment as KIKU_PRIVATE_WORDS (comma-separated), or keep .private-words in this checkout.\n"
    f"  Outside contributor without them: set {OVERRIDE}=1 to check the patterns only (CI still checks the words)."
)


def say(text):
    print(text, file=sys.stderr)


def ready():
    """Whether the words are there, or the documented override says to go without; says why not."""
    if privacy.has_words():
        return True
    if os.environ.get(OVERRIDE) == "1":
        say(f"kiku hooks: {OVERRIDE}=1: checking the patterns only, no private words")
        return True
    say(NO_WORDS)
    return False


def refuse_paths(paths, where):
    """Print each private path in `paths` and return whether there was one."""
    leaked = privacy.private_in(paths)
    for path in leaked:
        say(f"kiku hooks: {where}{path} is private (.gitignore); remove it (git rm --cached) instead of publishing it")
    return bool(leaked)


def gate_folder(folder, label):
    """Gate `folder`; True when clean."""
    if privacy.gate(folder) != 0:
        say(f"kiku hooks: the privacy gate refused {label}")
        return False
    return True


def pre_commit():
    """Refuse when the index holds a private path or text the gate rejects; the files gated are the index's own."""
    paths = privacy.git_files()
    if refuse_paths(paths, "") or not ready():
        return 1
    with tempfile.TemporaryDirectory(prefix="kiku-index-", ignore_cleanup_errors=True) as folder:
        privacy.git("checkout-index", "-a", "-f", f"--prefix={Path(folder).as_posix()}/")
        return 0 if gate_folder(folder, "the staged files") else 1


def tree(commit):
    """The (path, blob id) pairs of a commit's whole tree."""
    out = privacy.git("ls-tree", "-r", "-z", "--full-tree", commit).decode("utf-8", "surrogateescape")
    pairs = []
    for entry in filter(None, out.split("\0")):
        info, path = entry.split("\t", 1)
        _, kind, blob = info.split()
        if kind == "blob":
            pairs.append((path, blob))
    return pairs


def blobs(ids):
    """Contents of the blobs `ids`, from one `git cat-file --batch`."""
    out = privacy.git("cat-file", "--batch", input="".join(f"{i}\n" for i in ids).encode())
    found, at = {}, 0
    for _ in ids:
        end = out.index(b"\n", at)
        blob, _, size = out[at:end].split()
        at = end + 1 + int(size)
        found[blob.decode()] = out[end + 1 : at]
        at += 1
    return found


def metadata_problem(commit):
    """What in a commit's author, committer or message must not be published, or None. Words are never shown."""
    gate = privacy.load_gate()
    text = privacy.git("show", "-s", "--format=%an%n%ae%n%cn%n%ce%n%B", commit).decode("utf-8", "replace")
    lowered = text.lower()
    words = [w.strip().lower() for w in privacy.words().split(",") if len(w.strip()) >= gate.SHORTEST_WORD]
    if any(word in lowered for word in words):
        return "a private word in the author, committer or message"
    if re.search(gate.PATTERNS["a tool credited as an author"], text):
        return "a tool credited as an author in the message"
    return None


def commits_pushed(remote, local, remote_sha):
    """The commits a ref update would send: new to the remote, which is what it already has under this name and any ref."""
    try:
        base = [] if set(remote_sha) <= {"0"} else [f"^{remote_sha}"]
        return privacy.git("rev-list", "--reverse", local, *base, "--not", f"--remotes={remote}").decode().split()
    except SystemExit:  # the remote's commit is not here (a rewritten branch): everything the remote's refs lack
        return privacy.git("rev-list", "--reverse", local, "--not", f"--remotes={remote}").decode().split()


def check_commits(commits):
    """Refuse private paths, metadata and gated text in each commit; every commit is reported."""
    clean, seen = True, set()
    for commit in commits:
        short = commit[:10]
        pairs = tree(commit)
        if refuse_paths([path for path, _ in pairs], f"commit {short}: "):
            clean = False
        problem = metadata_problem(commit)
        if problem:
            say(f"kiku hooks: commit {short}: {problem}")
            clean = False
        new = [pair for pair in pairs if pair not in seen]
        seen.update(new)
        with tempfile.TemporaryDirectory(prefix="kiku-push-", ignore_cleanup_errors=True) as folder:
            contents = blobs(sorted({blob for _, blob in new}))
            for path, blob in new:
                target = Path(folder, path)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(contents[blob])
            if new and not gate_folder(folder, f"commit {short}"):
                clean = False
    return clean


def pre_push(remote, stdin=sys.stdin):
    """Refuse a push when any commit it sends fails check_commits. Branch deletions send nothing and pass."""
    if not ready():
        return 1
    commits = []
    for line in stdin:
        fields = line.split()
        if len(fields) != 4 or set(fields[1]) <= {"0"}:
            continue
        found = subprocess.run(["git", "rev-parse", "--verify", "-q", f"{fields[1]}^{{commit}}"], cwd=privacy.ROOT, capture_output=True, text=True)
        if found.returncode == 0:  # a tag of a tree or blob has no commit to check
            commits += [c for c in commits_pushed(remote, found.stdout.strip(), fields[3]) if c not in commits]
    return 0 if check_commits(commits) else 1


def install():
    """Point git at .githooks/ and say whether the check will have its words."""
    privacy.git("config", "core.hooksPath", ".githooks")
    for hook in (privacy.ROOT / ".githooks").iterdir():
        if os.name != "nt":
            hook.chmod(hook.stat().st_mode | 0o111)
    print("hooks: core.hooksPath is .githooks (pre-commit, pre-push)")
    if privacy.has_words():
        print("hooks: private words found; the check is complete")
    else:
        print(f"hooks: no private words yet: commits and pushes are refused until KIKU_PRIVATE_WORDS or .private-words has them (outside contributors: {OVERRIDE}=1)")
    return 0


def main(argv):
    command = argv[0] if argv else ""
    if command == "install":
        return install()
    if command == "pre-commit":
        return pre_commit()
    if command == "pre-push":
        return pre_push(argv[1] if len(argv) > 1 else "origin")
    sys.exit(__doc__)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
