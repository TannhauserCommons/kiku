"""The release candidate: `main`'s last green build, parked on one standing draft release.

Subcommands (see docs/context/contract.md): `base`, `record`, `fetch`, `publish`, `bump`.
Every one is safe to run again, so a failure anywhere costs a rerun and never a version:
the tag is created last, by publishing the draft.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import ghapi

# Ends the body of the one draft release that holds the candidate.
MARKER = "<!-- ci:candidate -->"
# Files of the release dir that are never release files: the manifests are handled apart,
# the notes become the body.
CANDIDATE_JSON = "candidate.json"  # what record built: name -> hash before signing
SIGNED_JSON = "signed.json"  # what publish uploaded: name -> every hash it signed to, over all attempts
NOT_ASSETS = (CANDIDATE_JSON, SIGNED_JSON, "NOTES.md")
# GitHub rewrites asset names it dislikes (spaces become dots, ...), which would break
# the name -> hash map, so record refuses names it would not keep as they are.
SAFE_NAME = re.compile(r"[A-Za-z0-9_+-][A-Za-z0-9._+-]*")
BUMP_ATTEMPTS = 3


class CandidateError(Exception):
    """A refusal with a message for the person reading the log."""


# --- process helpers (stubbed in tests) -------------------------------------------------


def git(*args):
    """Run git and return stdout; a non-zero exit raises with git's own message."""
    done = subprocess.run(["git", *args], capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {done.stderr.strip() or done.stdout.strip()}")
    return done.stdout


def git_ok(*args):
    """Whether a git command exits 0. For yes/no questions (`merge-base --is-ancestor`),
    so a "no" is never mistaken for an error by parsing its text."""
    return subprocess.run(["git", *args], capture_output=True).returncode == 0


def branch():
    """The release branch: env `CI_BRANCH`, else `main`. The workflow file stays `main.yml`."""
    return (os.environ.get("CI_BRANCH") or "").strip() or "main"


def require_bump_key(required=True):
    """Refuse unless the job has the deploy key for the bump.

    A push with the workflow's own token starts no workflow; the deploy key's push
    does, and that builds the next candidate. `fetch` asks first, so a missing key
    stops the release before anything irreversible, the tag, has happened. With
    `required=False` (fetch) only an explicit non-`true` value refuses: a project
    whose version is bumped elsewhere has no bump step and does not set the variable.
    """
    value = os.environ.get("BUMP_KEY_SET")
    if value != "true" and (required or value is not None):
        raise CandidateError(
            "BUMP_KEY is not set: the bump is pushed with a deploy key so that it starts main "
            '(README, "Copy it into a project")'
        )


def mise_version():
    """The version `main` carries: the last non-empty stdout line of `mise run version`.

    The last line, because mise and the project task may print progress before it.
    """
    mise = shutil.which("mise") or "mise"  # Windows needs the resolved .exe/.cmd
    done = subprocess.run([mise, "run", "version"], capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise RuntimeError(f"mise run version: {done.stderr.strip() or done.stdout.strip()}")
    lines = [line.strip() for line in done.stdout.splitlines() if line.strip()]
    if not lines:
        raise CandidateError("mise run version printed nothing")
    return lines[-1]


def mise_bump(version):
    """Have the project write `version` into every file that holds it."""
    mise = shutil.which("mise") or "mise"
    done = subprocess.run([mise, "run", "bump", version], capture_output=True, text=True, encoding="utf-8")
    if done.returncode != 0:
        raise RuntimeError(f"mise run bump {version}: {done.stderr.strip() or done.stdout.strip()}")


def switches(*args):
    """Run `switches.py <args>` and return its stdout (a separate script, so a subprocess)."""
    script = Path(__file__).with_name("switches.py")
    done = subprocess.run(
        [sys.executable, str(script), *args], capture_output=True, text=True, encoding="utf-8"
    )
    if done.returncode != 0:
        raise RuntimeError(f"switches.py {' '.join(args)}: {done.stderr.strip() or done.stdout.strip()}")
    return done.stdout


# --- GitHub lookups -------------------------------------------------------------------------


def all_releases():
    """Releases of the repository, drafts included (the token has write access)."""
    return ghapi.api("releases?per_page=100", paginate=True) or []


def standing_draft(releases=None):
    """The draft release that carries the candidate, or None.

    More than one is an error: two candidates mean nobody knows which one is real.
    """
    releases = all_releases() if releases is None else releases
    found = [
        r for r in releases if r.get("draft") and (r.get("body") or "").strip().endswith(MARKER)
    ]
    if len(found) > 1:
        names = ", ".join(f"{r.get('tag_name')} (id {r.get('id')})" for r in found)
        raise CandidateError(f"more than one candidate draft: {names}; delete the extra ones")
    return found[0] if found else None


def tag_ref(tag):
    """The ref object of the git tag (`{"ref", "object": {"sha", "type"}}`), or None.

    The prefix listing never 404s, unlike `git/ref/tags/<tag>`, and a prefix
    match (`v0.2.0-rc1` for `v0.2.0`) is filtered out here.
    """
    refs = ghapi.api(f"git/matching-refs/tags/{tag}?per_page=100") or []
    return next((ref for ref in refs if ref.get("ref") == f"refs/tags/{tag}"), None)


def tag_exists(tag):
    """Whether the git tag exists."""
    return tag_ref(tag) is not None


def tag_commit(tag):
    """The commit a tag points at: lightweight tags point straight at it, annotated ones via a tag object."""
    obj = tag_ref(tag)["object"]
    if obj.get("type") == "tag":
        obj = ghapi.api(f"git/tags/{obj['sha']}")["object"]
    return obj["sha"]


def draft_assets(release_id):
    """The assets currently on a release."""
    return ghapi.api(f"releases/{release_id}/assets?per_page=100", paginate=True) or []


# --- files -------------------------------------------------------------------------------------


def sha256(path):
    """Hex SHA-256 of a file, read in chunks."""
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def release_files(directory):
    """The regular files directly in `directory` that are release files, sorted by name.

    No recursion: assets are flat, so a subdirectory is an error rather than silently skipped.
    """
    root = Path(directory)
    if not root.is_dir():
        raise CandidateError(f"{root} is not a directory")
    files = []
    for entry in sorted(root.iterdir(), key=lambda e: e.name):
        if entry.is_dir():
            raise CandidateError(f"{entry} is a directory; release files must sit directly in {root}")
        if not entry.is_file():
            raise CandidateError(f"{entry} is not a regular file")
        if entry.name not in NOT_ASSETS:
            files.append(entry)
    return files


def read_manifest(directory):
    """`candidate.json` of `directory` as a dict; raises CandidateError when unusable."""
    path = Path(directory) / "candidate.json"
    if not path.is_file():
        raise CandidateError(f"{path} is missing")
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as err:
        raise CandidateError(f"{path} is not valid JSON: {err}") from err
    problems = manifest_problems(manifest)
    if problems:
        raise CandidateError(f"{path}: " + "; ".join(problems))
    return manifest


def manifest_problems(manifest):
    """What is wrong with a parsed manifest (empty list when it is usable)."""
    if not isinstance(manifest, dict):
        return ["not a JSON object"]
    problems = []
    for key in ("sha", "version"):
        if not isinstance(manifest.get(key), str) or not manifest.get(key):
            problems.append(f"missing `{key}`")
    if not isinstance(manifest.get("files"), dict):
        problems.append("missing `files`")
    return problems


# --- notes ---------------------------------------------------------------------------------


def split_subject(subject):
    """`(area, text)` of a commit subject: the prefix before the first `: ` when short, else Other."""
    prefix, sep, rest = subject.partition(": ")
    if sep and rest.strip() and prefix.strip() and len(prefix) <= 40 and len(prefix.split()) <= 3:
        return prefix, rest.strip()
    return "Other", subject


def group_subjects(subjects):
    """Subjects (newest first) as `[(area, [text, ...])]`: areas by first appearance, Other last."""
    groups = {}
    for subject in subjects:
        area, text = split_subject(subject)
        groups.setdefault(area, []).append(text)
    other = groups.pop("Other", None)
    ordered = list(groups.items())
    if other:
        ordered.append(("Other", other))
    return ordered


def build_notes(version, sha, last_tag, subjects, switch_notes="", extra_notes=""):
    """The draft's Markdown body. The last two lines (candidate sha, marker) identify the draft."""
    parts = [f"## v{version}"]
    extras = [e.strip() for e in (switch_notes, extra_notes) if e.strip()]
    if subjects:
        for area, items in group_subjects(subjects):
            parts.append(f"### {area}\n" + "\n".join(f"- {item}" for item in items))
    elif not extras:
        # Only when there is nothing else to say; other notes make the line noise.
        parts.append(f"No changes since {last_tag}." if last_tag else "No changes.")
    parts += extras
    return "\n\n".join(parts) + f"\n\nCandidate: {sha}\n{MARKER}\n"


def add_release_note(body, note):
    """`body` with `note` as its own paragraph right after the first line (the heading).

    The note is the owner's own text and goes in as it is, not interpreted.
    """
    heading, _, rest = body.partition("\n")
    rest = rest.lstrip("\n")
    return f"{heading}\n\n{note}\n" + (f"\n{rest}" if rest else "")


def strip_candidate_lines(body):
    """The body without its two trailing identity lines, for the published release."""
    lines = (body or "").rstrip().splitlines()
    if lines and lines[-1].strip() == MARKER:
        lines.pop()
    if lines and lines[-1].startswith("Candidate: "):
        lines.pop()
    return "\n".join(lines).rstrip() + "\n"


def last_release_tag(sha):
    """The newest `v*` tag reachable from `sha`, or None when there is none."""
    try:
        return git("describe", "--tags", "--abbrev=0", "--match", "v*", sha).strip() or None
    except RuntimeError:  # git describe fails when no tag matches
        return None


def names_only_the_version(subject, version):
    """Whether a subject is one or two words followed by this version (`Start v0.1.1`, `Kiku 0.3.2`).

    Such a commit only moves the version number, so it is not a change to list.
    Another version in the subject does not match: that is a real commit.
    """
    pattern = rf"(?:\S+\s+){{1,2}}v?{re.escape(version)}"
    return re.fullmatch(pattern, subject.strip()) is not None


def commit_subjects(sha, last_tag, version):
    """Subjects of the non-merge commits since the last release, newest first."""
    span = f"{last_tag}..{sha}" if last_tag else sha
    subjects = git("log", "--no-merges", "--format=%s", span).splitlines()
    return [s for s in subjects if s.strip() and not names_only_the_version(s, version)]


# --- subcommands -----------------------------------------------------------------------------


def cmd_base(args):
    """Output `base`: the commit the release branch last tested green; nothing when there is none."""
    runs = ghapi.api(
        f"actions/workflows/main.yml/runs?branch={branch()}&status=success&per_page=1"
    ) or {}
    found = runs.get("workflow_runs") or []
    if found:
        ghapi.output("base", found[0]["head_sha"])
    return 0


def replace_assets(draft, tag, files, manifest_path):
    """Swap the draft's assets for `files`, ordered so a half-done swap can never pass `fetch`.

    The manifest goes first on the way out and last on the way in: with no manifest
    on the draft, `fetch` refuses, so a crash in between costs a rerun only.
    """
    # Every old asset goes, a stale signed.json included: it would make `fetch`
    # accept signed hashes that belong to a different candidate.
    old = sorted(draft_assets(draft["id"]), key=lambda a: a["name"] != CANDIDATE_JSON)
    for asset in old:
        ghapi.api(f"releases/assets/{asset['id']}", "DELETE")
    if files:
        ghapi.gh("release", "upload", tag, *[str(f) for f in files], "--clobber")
    ghapi.gh("release", "upload", tag, str(manifest_path), "--clobber")


def cmd_record(args):
    """Make `dist/` the release candidate on the standing draft."""
    version = mise_version()
    tag = f"v{version}"
    sha = os.environ.get("GITHUB_SHA") or git("rev-parse", "HEAD").strip()
    if tag_exists(tag):
        # Between the released commit and the release's bump commit, main may take
        # more commits that still carry this version. Their runs must stay green,
        # so this is not a failure; the release moves main on, and the next green
        # main records the next candidate.
        print(
            f"candidate: {tag} is already released; this commit is not a candidate "
            f"(the release moves {branch()} to the next version)"
        )
        print(f"::notice title=No candidate::{tag} is already released")
        return 0
    files = release_files(args.dir)
    if not files:
        raise CandidateError(f"no release files in {args.dir}")
    bad = [f.name for f in files if not SAFE_NAME.fullmatch(f.name)]
    if bad:
        raise CandidateError(
            "file names GitHub would rewrite as assets (use letters, digits, . _ + -): "
            + ", ".join(bad)
        )

    manifest = {
        "sha": sha,
        "version": version,
        "run_id": int(os.environ.get("GITHUB_RUN_ID") or 0),
        "run_attempt": int(os.environ.get("GITHUB_RUN_ATTEMPT") or 0),
        "files": {f.name: sha256(f) for f in files},
    }
    manifest_path = Path(args.dir) / "candidate.json"
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    last_tag = last_release_tag(sha)
    notes_file = Path(args.dir) / "NOTES.md"
    body = build_notes(
        version,
        sha,
        last_tag,
        commit_subjects(sha, last_tag, version),
        switches("notes"),
        notes_file.read_text(encoding="utf-8") if notes_file.is_file() else "",
    )

    fields = {
        "tag_name": tag,
        "name": tag,
        "target_commitish": sha,
        "body": body,
        "draft": True,
    }
    draft = standing_draft()
    if draft is None:
        draft = ghapi.api("releases", "POST", fields)
    else:
        draft = ghapi.api(f"releases/{draft['id']}", "PATCH", fields)
    replace_assets(draft, tag, files, manifest_path)
    print(f"candidate: {tag} {sha[:7]} ({len(files)} files)")
    return 0


def read_signed(root, candidate_sha):
    """`(signed, problems)`: `signed.json` normalised to `{"sha", "files": {name: [hash, ...]}}`.

    `signed` is None when the file is absent or unusable. A plain string for a name
    (the earlier shape) counts as a one-item list. A signer that is not
    byte-deterministic gives a new hash on every attempt, so the lists grow.
    """
    path = root / SIGNED_JSON
    if not path.is_file():
        return None, []
    try:
        signed = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as err:
        return None, [f"{SIGNED_JSON} is not valid JSON: {err}"]
    if not isinstance(signed, dict) or not isinstance(signed.get("files"), dict):
        return None, [f"{SIGNED_JSON} is malformed"]
    if signed.get("sha") != candidate_sha:
        return None, [
            f"{SIGNED_JSON} sha {str(signed.get('sha'))[:7]} differs from {CANDIDATE_JSON} "
            f"{candidate_sha[:7]}"
        ]
    files = {}
    for name, hashes in signed["files"].items():
        if isinstance(hashes, str):
            hashes = [hashes]
        if not isinstance(hashes, list) or not all(isinstance(h, str) for h in hashes):
            return None, [f"{SIGNED_JSON} is malformed: {name}"]
        files[name] = hashes
    return {"sha": signed["sha"], "files": files}, []


def verify_candidate(directory, draft):
    """Every way the downloaded files disagree with the manifests and the draft, as messages.

    With a `signed.json` an earlier release attempt got as far as uploading signed
    files, so a listed file may carry either hash; any other file must be one that
    attempt uploaded, in the state it uploaded it.
    """
    root = Path(directory)
    problems = []
    present = {e.name for e in root.iterdir() if e.name not in (CANDIDATE_JSON, SIGNED_JSON)}
    path = root / CANDIDATE_JSON
    if not path.is_file():
        return [f"{CANDIDATE_JSON} is missing"] + [f"unlisted file: {n}" for n in sorted(present)], None
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except ValueError as err:
        return [f"{CANDIDATE_JSON} is not valid JSON: {err}"], None
    bad = manifest_problems(manifest)
    if bad:
        return [f"{CANDIDATE_JSON}: {b}" for b in bad], None

    signed, signed_problems = read_signed(root, manifest["sha"])
    problems += signed_problems
    signed_files = signed["files"] if signed else {}
    listed = manifest["files"]
    for name in sorted(listed):
        file = root / name
        if not file.is_file():
            problems.append(f"missing file: {name}")
        elif sha256(file) != listed[name] and sha256(file) not in signed_files.get(name, []):
            problems.append(f"hash differs: {name}")
    for name in sorted(present - set(listed)):
        file = root / name
        if name not in signed_files:
            problems.append(f"unlisted file: {name}")
        elif sha256(file) not in signed_files[name]:
            problems.append(f"hash differs: {name}")
    if manifest["sha"] != draft.get("target_commitish"):
        problems.append(
            f"{CANDIDATE_JSON} sha {manifest['sha'][:7]} differs from the draft's "
            f"{str(draft.get('target_commitish'))[:7]}"
        )
    if draft.get("tag_name") != f"v{manifest['version']}":
        problems.append(
            f"{CANDIDATE_JSON} version {manifest['version']} differs from the draft's tag "
            f"{draft.get('tag_name')}"
        )
    return problems, manifest


def resume_published(version, releases):
    """Finish the report of a release that published but did not finish; None when this is no resume.

    The version the checked-out tree carries decides, not what drafts exist: a rerun
    checks out the same commit as the original run, so while the bump has not
    landed the tree still carries the released version. A new candidate draft that
    appeared meanwhile must not be released by a rerun that only finishes the old
    release. Then only the bump is left, and its inputs are read from the
    published release instead of a download.
    """
    tag = f"v{version}"
    published = next(
        (r for r in releases if r.get("tag_name") == tag and not r.get("draft")), None
    )
    if published is None or not tag_exists(tag):
        return None
    print(f"release: {tag} is already published; only the bump is left")
    # The job may have died right after the publish, before this cleanup; nothing else retries it.
    drop_manifests(published["id"], tag, dry_run=bool(os.environ.get("DRY_RUN")))
    ghapi.output("published", "true")
    ghapi.output("sha", tag_commit(tag))
    ghapi.output("version", version)
    ghapi.output("tag", tag)
    return 0


def cmd_fetch(args):
    """Check the candidate out as a source tree with its files in `<tree>/dist`.

    Downloads into a scratch directory and verifies there, so a failed check leaves
    nothing behind; the worktree is added only once everything has passed. The
    project's own `publish` task then runs from that tree, so the signing task
    always matches the files it signs.

    Always sets the output `published`: `true` only on the resume path.
    """
    require_bump_key(required=False)  # before any GitHub call; absent means no bump step
    version = mise_version()  # the tree's own version, before any release is looked at
    releases = all_releases()
    resumed = resume_published(version, releases)
    if resumed is not None:
        return resumed
    draft = standing_draft(releases)
    if draft is None:
        raise CandidateError(f"no candidate: {branch()} has not been green since the last release")
    tree = Path(args.tree)
    scratch = Path(tempfile.mkdtemp(prefix="candidate-"))
    try:
        ghapi.gh("release", "download", draft["tag_name"], "--dir", str(scratch))
        problems, manifest = verify_candidate(scratch, draft)
        if problems:
            for problem in problems:
                print(f"fetch: {problem}", file=sys.stderr)
            raise CandidateError(f"the candidate failed {len(problems)} check(s)")
        sha = manifest["sha"]
        # The candidate must be a commit of main: a draft can only be as trustworthy as
        # the history it claims. The yes/no answer is the exit code of merge-base.
        git("fetch", "origin", branch())
        if not git_ok("merge-base", "--is-ancestor", sha, f"origin/{branch()}"):
            raise CandidateError(f"the candidate {sha[:7]} is not on {branch()}")
        if tree.exists():
            raise CandidateError(f"{tree} already exists; remove it and run fetch again")
        git("worktree", "add", "--detach", str(tree), sha)
        dist = tree / "dist"
        if dist.exists() and any(dist.iterdir()):
            git("worktree", "remove", "--force", str(tree))
            raise CandidateError(
                f"{dist} is tracked and not empty: a project that commits a dist/ folder "
                "cannot use this release layout"
            )
        dist.mkdir(parents=True, exist_ok=True)
        for entry in sorted(scratch.iterdir()):
            shutil.move(str(entry), str(dist / entry.name))
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    ghapi.output("published", "false")
    ghapi.output("sha", manifest["sha"])
    ghapi.output("version", manifest["version"])
    ghapi.output("tag", f"v{manifest['version']}")
    return 0


def cmd_publish(args):
    """Upload the signed files and publish the draft; publishing creates the tag, so it goes last."""
    manifest = read_manifest(args.dir)
    version, sha = manifest["version"], manifest["sha"]
    tag = f"v{version}"
    draft = standing_draft()
    if draft is None:
        if tag_exists(tag):
            # A rerun after the publish already happened: nothing left to do here.
            print(f"already published: {tag}")
            return 0
        raise CandidateError(f"no candidate draft and no tag {tag}: nothing to publish")
    if draft.get("tag_name") != tag or draft.get("target_commitish") != sha:
        # A newer green main replaced the candidate while this release ran; its
        # assets are not the ones that were signed.
        raise CandidateError(
            f"the draft is {draft.get('tag_name')} at {str(draft.get('target_commitish'))[:7]}, "
            f"but {args.dir}/candidate.json says {tag} at {sha[:7]}; rerun the release"
        )
    files = release_files(args.dir)
    if not files:
        raise CandidateError(f"no release files in {args.dir}")

    # Earlier attempts' hashes stay valid: a signer that is not byte-deterministic leaves
    # files from attempt 1 on the draft that match neither candidate.json nor attempt 2.
    earlier, problems = read_signed(Path(args.dir), sha)
    if problems:
        raise CandidateError("; ".join(problems))
    names = ", ".join(f.name for f in files)
    # The owner's own words for this release, passed through the environment by the workflow.
    note = (os.environ.get("RELEASE_NOTES") or "").strip()
    if args.dry_run:
        print(f"dry run: would upload {SIGNED_JSON}, then {len(files)} files to {tag}: {names}")
        print("dry run: would run switches.py shipped")
        if note:
            print(f"dry run: would add the release note ({len(note)} characters)")
        print(f"dry run: would publish {tag} (creates the tag at {sha[:7]})")
        print(f"dry run: would delete {CANDIDATE_JSON} and {SIGNED_JSON} from the release")
        return 0
    # signed.json goes up before any file is replaced: if this run dies half-way, the
    # draft then holds files in two states, and `fetch` can only tell which are
    # legitimate (signed) from the hashes this manifest recorded.
    signed_path = Path(args.dir) / SIGNED_JSON
    hashes = {name: list(old) for name, old in (earlier or {"files": {}})["files"].items()}
    for f in files:
        now = sha256(f)
        if now not in hashes.setdefault(f.name, []):
            hashes[f.name].append(now)
    signed = {"sha": sha, "files": dict(sorted(hashes.items()))}
    signed_path.write_text(json.dumps(signed, indent=2) + "\n", encoding="utf-8")
    ghapi.gh("release", "upload", draft["tag_name"], str(signed_path), "--clobber")
    ghapi.gh("release", "upload", draft["tag_name"], *[str(f) for f in files], "--clobber")
    switches("shipped")
    body = strip_candidate_lines(draft.get("body"))
    if note:
        body = add_release_note(body, note)
    ghapi.api(f"releases/{draft['id']}", "PATCH", {"draft": False, "body": body})
    drop_manifests(draft["id"], tag)
    print(f"published: {tag} {sha[:7]} ({len(files)} files)")
    return 0


def drop_manifests(release_id, tag, dry_run=False):
    """Delete the manifests from the published release: they describe the candidate, not the release.

    The release is out by now, so a failure here warns and never fails the step.
    A dry run only says what it would remove.
    """
    try:
        found = [a for a in draft_assets(release_id) if a["name"] in (CANDIDATE_JSON, SIGNED_JSON)]
        if found and dry_run:
            print(f"dry run: would remove the manifests from {tag}")
            return
        for asset in found:
            ghapi.api(f"releases/assets/{asset['id']}", "DELETE")
    except RuntimeError as err:
        print(f"::warning title=Manifests left on {tag}::could not delete {CANDIDATE_JSON} and {SIGNED_JSON}: {err}")


def next_patch(version):
    """`x.y.z` -> `x.y.(z+1)`; any other shape is an error, never a guess."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", version)
    if not match:
        raise CandidateError(f"cannot bump version {version!r}: not x.y.z")
    major, minor, patch = match.groups()
    return f"{major}.{minor}.{int(patch) + 1}"


def cmd_bump(args):
    """Move the release branch to the version after the one just released."""
    released = args.released.removeprefix("v")
    nxt = next_patch(released)
    if not args.dry_run:
        require_bump_key()  # fetch asked already; this is the step that needs the key
    if args.dry_run:
        # Read the tree as it is: a dry run must not touch the checkout.
        carried = mise_version()
        if carried != released:
            print(f"{branch()} already carries {carried}; nothing to bump")
        else:
            print(f"dry run: would bump {branch()} to {nxt}")
        return 0
    for attempt in range(1, BUMP_ATTEMPTS + 1):
        # Start from the real tip of the branch each time: a rejected push means it moved.
        git("fetch", "origin", branch())
        git("checkout", "--detach", f"origin/{branch()}")
        carried = mise_version()
        if carried != released:
            print(f"{branch()} already carries {carried}; nothing to bump")
            return 0
        mise_bump(nxt)
        git("commit", "-am", f"Start v{nxt}")
        try:
            git("push", "origin", f"HEAD:{branch()}")
        except RuntimeError as err:
            if attempt == BUMP_ATTEMPTS:
                raise CandidateError(f"push to {branch()} failed {BUMP_ATTEMPTS} times: {err}") from err
            print(f"push rejected (attempt {attempt}); {branch()} moved, trying again")
            continue
        print(f"bumped {branch()} to {nxt}")
        return 0
    return 1  # unreachable: the loop returns or raises


def parser():
    """The argparse tree: one subcommand per step."""
    top = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = top.add_subparsers(dest="command", required=True)
    sub.add_parser("base", help="print the last commit main tested green").set_defaults(run=cmd_base)
    record = sub.add_parser("record", help="put the build on the standing draft")
    record.add_argument("--dir", default="dist")
    record.set_defaults(run=cmd_record)
    fetch = sub.add_parser("fetch", help="check the candidate out as a source tree with its files")
    fetch.add_argument("--tree", default="candidate", help="where the source tree goes (must not exist)")
    fetch.set_defaults(run=cmd_fetch)
    publish = sub.add_parser("publish", help="upload signed files and publish the draft")
    publish.add_argument("--dir", default="candidate/dist")
    publish.add_argument("--dry-run", action="store_true")
    publish.set_defaults(run=cmd_publish)
    bump = sub.add_parser("bump", help="move main to the next version")
    bump.add_argument("--released", required=True, help="the version just released")
    bump.add_argument("--dry-run", action="store_true")
    bump.set_defaults(run=cmd_bump)
    return top


def main(argv=None):
    """Run one subcommand; a refusal prints one message and exits 1."""
    args = parser().parse_args(argv)
    try:
        return args.run(args)
    except (CandidateError, RuntimeError) as err:
        print(f"candidate: {err}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
