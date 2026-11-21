"""After a `check` or `main` run: one infra rerun, flake issues, the red-`main` issue.

Run as `python .github/ci/after.py <run_id>`. The order is fixed by the contract
(docs/context/contract.md, section `after.py`): cancelled runs are ignored, an
infra-only failure is rerun once, flakes are filed for any other run, and a push
to `main` opens, updates or closes the one `main-red` issue. A run whose code
came from another repository (a fork's pull request) files no flake issues.
"""

import argparse
import os
import sys
import tempfile
import unicodedata
import xml.etree.ElementTree as ET

import ghapi

FLAKE_LABEL = "flaky"
RED_LABEL = "main-red"
RED_TITLE = "main is red"
NEW_ISSUE_CAP = 10
TEST_ID_MAX = 200
COMMIT_LINES_MAX = 50
RULE = "Fix or revert within the window; revert by default."

# JUnit children that mark a pass-on-retry, and the ones that mean it really failed.
JUNIT_FLAKY = {"flakyFailure", "flakyError", "rerunFailure", "rerunError"}
JUNIT_FAILED = {"failure", "error"}


def branch():
    """The release branch: env `CI_BRANCH`, else `main`. Empty counts as unset.

    The issue title and label stay `main is red` / `main-red` whatever this says:
    other tools look those up by name.
    """
    return os.environ.get("CI_BRANCH", "").strip() or "main"


# --- text from outside -------------------------------------------------------


def clean(text, limit=TEST_ID_MAX):
    """Make untrusted text safe to print and to put in an issue.

    Drops control, format (bidi tricks) and line-separator characters, trims, and
    caps the length. Why: test ids and commit text come from artifacts and
    authors; they are never run, but they do end up in titles and comments.
    """
    kept = "".join(
        ch for ch in str(text) if unicodedata.category(ch) not in ("Cc", "Cf", "Zl", "Zp")
    )
    return kept.strip()[:limit]


def md_code(text):
    """Inline code span that the text itself cannot break out of.

    Why: a backtick in a test name must not turn the rest of the issue into
    live Markdown (mentions, links).
    """
    longest = run = 0
    for ch in text:
        run = run + 1 if ch == "`" else 0
        longest = max(longest, run)
    fence = "`" * (longest + 1)
    return f"{fence} {text} {fence}"


# --- the run and its jobs ----------------------------------------------------


def workflow_file(run):
    """File name of the run's workflow, from `path` (not the display name).

    The API may append `@refs/...` to the path; that part is not the file.
    """
    path = (run.get("path") or "").split("@", 1)[0]
    return path.rsplit("/", 1)[-1]


def head_repo(run):
    """`owner/name` the run's code came from, as the API reports it, or "".

    Empty when the API has none (a deleted fork), which counts as foreign.
    """
    return (run.get("head_repository") or {}).get("full_name") or ""


def is_foreign(run):
    """Whether the run's code came from a repository other than this one.

    A fork's pull request runs the fork's own copy of `check.yml`, so whatever
    that run uploaded is the fork author's text. Decided from the run object
    GitHub returns, never from anything the run wrote.
    """
    return head_repo(run).lower() != ghapi.repo().lower()


def get_jobs(run):
    """Jobs of the run's latest attempt (a `workflow_run` event is for that attempt)."""
    attempt = run.get("run_attempt", 1)
    data = ghapi.api(f"actions/runs/{run['id']}/attempts/{attempt}/jobs?per_page=100")
    return (data or {}).get("jobs", [])


def failed_jobs(jobs):
    """Jobs that failed. Cancelled and skipped jobs are neither red nor green."""
    return [job for job in jobs if job.get("conclusion") == "failure"]


def stopped_jobs(jobs):
    """Jobs that were cancelled after they had started: timed out, or stopped by hand.

    A run that was replaced while it waited never started a job, so it has none.
    """
    return [
        {**job, "stopped": True}
        for job in jobs
        if job.get("conclusion") == "cancelled"
        and any(step.get("conclusion") == "success" for step in job.get("steps") or [])
    ]


def is_gate(job):
    """The gate fails because another job did; it says nothing about the cause."""
    name = job.get("name") or ""
    return name == "gate" or name.endswith(" / gate")


def causes(failed):
    """The failed jobs that can explain a red run: all of them but the gate.

    When only the gate failed (a job it needs was cancelled or timed out, or the
    gate itself broke) the gate is the cause.
    """
    return [job for job in failed if not is_gate(job)] or failed


def infra_reason(job):
    """Why this failed job is an infra failure, or None when it is a real one.

    By step position only, never by log text: infra is a failure that happened
    before the project's own command ran (checkout, tool install, runner setup)
    or one that left no failed step (a lost runner).
    """
    steps = job.get("steps") or []
    failed = sorted((s for s in steps if s.get("conclusion") == "failure"), key=lambda s: s["number"])
    if not failed:
        return "no step failed (lost runner)"
    first_failed = failed[0]
    commands = [s["number"] for s in steps if s["name"].startswith("mise run ")]
    if commands:
        return first_failed["name"] if first_failed["number"] < min(commands) else None
    # Without a project command the only failure that is surely not ours is
    # GitHub's own job setup.
    return first_failed["name"] if first_failed["name"] == "Set up job" else None


def rerun_if_infra(run, failed):
    """Rerun the failed jobs once when every one of them is an infra failure.

    Returns True when it did. Why only on attempt 1: a second infra failure is
    not luck any more, so it is reported like any other failure.
    """
    if run.get("run_attempt", 1) != 1 or not failed:
        return False
    reasons = [(job, infra_reason(job)) for job in causes(failed)]
    if any(reason is None for _, reason in reasons):
        return False
    ghapi.api(f"actions/runs/{run['id']}/rerun-failed-jobs", "POST")
    for job, reason in reasons:
        print(f"infra rerun: {clean(job['name'])}: {clean(reason)}")
    return True


# --- flakes ------------------------------------------------------------------


def junit_flaky_ids(path):
    """Flaky test ids in one JUnit file: retried-and-passed, never failed in the end."""
    try:
        root = ET.parse(path).getroot()
    except (ET.ParseError, OSError) as err:
        print(f"flakes: cannot read {os.path.basename(path)}: {err}", file=sys.stderr)
        return []
    ids = []
    for case in root.iter():
        if case.tag.rsplit("}", 1)[-1] != "testcase":
            continue
        kinds = {child.tag.rsplit("}", 1)[-1] for child in case}
        if kinds & JUNIT_FLAKY and not kinds & JUNIT_FAILED:
            classname, name = case.get("classname"), case.get("name") or ""
            ids.append(f"{classname}::{name}" if classname else name)
    return ids


def txt_flaky_ids(path):
    """One test id per line, for runners that cannot write JUnit flake marks."""
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return handle.read().splitlines()
    except OSError as err:
        print(f"flakes: cannot read {os.path.basename(path)}: {err}", file=sys.stderr)
        return []


def read_flake_dir(folder):
    """Every flaky test id under `folder`, cleaned and deduplicated, in file order."""
    found = {}
    for where, dirs, files in os.walk(folder):
        dirs.sort()
        for name in sorted(files):
            path = os.path.join(where, name)
            if name.endswith(".xml"):
                raw = junit_flaky_ids(path)
            elif name.endswith(".txt"):
                raw = txt_flaky_ids(path)
            else:
                continue
            for test_id in raw:
                test_id = clean(test_id)
                if test_id:
                    found[test_id] = True
    return list(found)


def collect_flakes(run_id):
    """Flaky test ids from the run's `flakes-*` artifacts.

    Lists the artifacts first: `gh run download` fails when none match, and that
    failure must not be confused with a real download error.
    """
    listing = ghapi.api(f"actions/runs/{run_id}/artifacts?per_page=100") or {}
    wanted = [
        a for a in listing.get("artifacts", [])
        if a["name"].startswith("flakes-") and not a.get("expired")
    ]
    if not wanted:
        return []
    with tempfile.TemporaryDirectory() as tmp:
        ghapi.gh("run", "download", str(run_id), "--pattern", "flakes-*", "--dir", tmp)
        return read_flake_dir(tmp)


def report_flakes(run, test_ids):
    """Open or comment on one `flaky: <id>` issue per test, at most 10 new per run."""
    url = run["html_url"]
    sha = run["head_sha"]
    workflow = f"{run.get('name') or workflow_file(run)} ({run.get('path') or '?'})"
    opened = commented = skipped = 0
    for test_id in test_ids:
        title = f"flaky: {test_id}"
        body = (
            f"Passed only on retry.\n\n"
            f"- Test: {md_code(test_id)}\n- Run: {url}\n"
            f"- Workflow: {workflow}\n- Commit: {sha}\n"
        )
        comment = f"Flaked again: {url} at {sha[:7]}"
        # Look before creating so the cap only counts issues that would be new.
        if not ghapi.find_issue(FLAKE_LABEL, title) and opened >= NEW_ISSUE_CAP:
            skipped += 1
            continue
        number, created = ghapi.upsert_issue(FLAKE_LABEL, title, body, comment)
        if created:
            opened += 1
        else:
            commented += 1
        print(f"flaky: {test_id}: {'opened' if created else 'commented on'} #{number}")
    if skipped:
        print(f"flaky: skipped {skipped} new issues (cap {NEW_ISSUE_CAP} per run)")
    return opened, commented, skipped


# --- red and green main ------------------------------------------------------


def last_green_sha(run):
    """head_sha of the newest successful main run before this one, or None.

    Asks for a page, not one run: a newer run may already be green, and a rerun
    of the same commit is not a different base.
    """
    data = ghapi.api(
        f"actions/workflows/{workflow_file(run)}/runs?branch={branch()}&status=success&per_page=20"
    )
    for other in (data or {}).get("workflow_runs", []):
        if other["id"] < run["id"] and other["head_sha"] != run["head_sha"]:
            return other["head_sha"]
    return None


def commit_line(sha, author, message):
    """`short sha  author  subject`, as it goes into the issue."""
    subject = (message or "").splitlines()[0] if message else ""
    return f"{sha[:7]}  {clean(author or '?', 80)}  {clean(subject)}"


def commit_range(run, base):
    """The range text and one line per commit since the last green main run.

    A compare that fails (a base lost to a force-push) falls back to the head
    commit: a red issue without the full range beats no red issue.
    """
    head = run["head_sha"]
    head_commit = run.get("head_commit") or {}
    own = commit_line(head, (head_commit.get("author") or {}).get("name"), head_commit.get("message"))
    if not base:
        return head[:7], [own]
    try:
        data = ghapi.api(f"compare/{base}...{head}")
    except RuntimeError as err:
        print(f"main-red: compare failed, listing the head commit only: {err}", file=sys.stderr)
        return f"{base[:7]}..{head[:7]}", [own]
    lines = [
        commit_line(c["sha"], (c.get("commit", {}).get("author") or {}).get("name"),
                    c.get("commit", {}).get("message"))
        for c in (data or {}).get("commits", [])
    ]
    if len(lines) > COMMIT_LINES_MAX:
        lines = lines[:COMMIT_LINES_MAX] + [f"... and {len(lines) - COMMIT_LINES_MAX} more"]
    return f"{base[:7]}..{head[:7]}", lines or [own]


def failed_job_text(failed):
    """The failed job names for an issue, or why there are none."""
    if not failed:
        return "none recorded (the run was cancelled or timed out)"
    return ", ".join(
        md_code(clean(job["name"])) + (" (timed out or cancelled)" if job.get("stopped") else "")
        for job in causes(failed)
    )


def report_red(run, failed):
    """Open the `main is red` issue, or comment on the open one."""
    rng, lines = commit_range(run, last_green_sha(run))
    jobs_text = failed_job_text(failed)
    # A commit line starts with a sha, so no line can close the fence.
    commits = "```\n" + "\n".join(lines) + "\n```"
    url = run["html_url"]
    body = (
        f"{branch()} is red.\n\n- Run: {url}\n- Failed jobs: {jobs_text}\n- Range: `{rng}`\n\n"
        f"Commits:\n\n{commits}\n\n{RULE}\n"
    )
    comment = (
        f"Still red: {url}\n\n- Failed jobs: {jobs_text}\n- Range: `{rng}`\n\n"
        f"Commits:\n\n{commits}\n"
    )
    number, created = ghapi.upsert_issue(RED_LABEL, RED_TITLE, body, comment)
    print(f"main-red: {'opened' if created else 'commented on'} #{number} ({rng})")


def is_main_run(run):
    """A `main.yml` run for a push to the release branch.

    A push run of the same workflow on another branch is not the release
    branch's run, so it must not open or close the issue.
    """
    return (
        workflow_file(run) == "main.yml"
        and run.get("event") == "push"
        and run.get("head_branch") == branch()
    )


def is_newest_main_run(run):
    """Whether no later `main` run has finished since this one.

    A rerun of an old run, or a slow `after`, must not reopen or close the
    issue against what a newer run already said about main.
    """
    data = ghapi.api(
        f"actions/workflows/{workflow_file(run)}/runs?branch={branch()}&status=completed&per_page=10"
    )
    runs = (data or {}).get("workflow_runs", [])
    # Any newer finished run counts, cancelled ones too: one may be a timeout
    # that is red, and one replaced while it waited has a successor that will
    # report in its place.
    return not runs or runs[0]["id"] <= run["id"]


def report_green(run):
    """Close the open `main-red` issue, naming the commit and run that fixed it."""
    issue = ghapi.find_issue(RED_LABEL)
    if not issue:
        return
    ghapi.close_issue(
        issue["number"], f"{branch()} is green again at {run['head_sha'][:7]}: {run['html_url']}"
    )
    print(f"main-red: closed #{issue['number']}")


# --- entry point -------------------------------------------------------------


def main(argv=None):
    """Handle one finished run. Exit 0 unless the script itself breaks."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("run_id", type=int, help="id of the finished workflow run")
    args = parser.parse_args(argv)
    try:
        return handle(args.run_id)
    except RuntimeError as err:
        print(f"after: {err}", file=sys.stderr)
        return 1


def handle(run_id):
    """The four steps of the contract, in order."""
    run = ghapi.api(f"actions/runs/{run_id}")
    conclusion = run.get("conclusion")
    if conclusion in ("skipped", None):
        print(f"after: run {run_id} is {conclusion or 'not finished'}; nothing to do")
        return 0

    failed = []
    if conclusion == "cancelled":
        # GitHub concludes a run "cancelled" when a job hit its timeout, the
        # same as when someone cancelled it. On a PR the gate's own failed
        # check blocks the merge. On main nothing else would say that a build
        # hung, so a main run that was stopped after it had started is red.
        jobs = get_jobs(run) if is_main_run(run) else []
        failed = failed_jobs(jobs) + stopped_jobs(jobs)
        if not failed:
            print(f"after: run {run_id} is cancelled; nothing to do")
            return 0
        conclusion = "failure"
    elif conclusion == "failure":
        failed = failed_jobs(get_jobs(run))
        if rerun_if_infra(run, failed):
            return 0

    if is_foreign(run):
        print(f"flakes: run {run_id} is from {clean(head_repo(run)) or 'an unknown repository'}, "
              f"not {ghapi.repo()}; its artifacts are not read")
    else:
        flakes = collect_flakes(run_id)
        if flakes:
            report_flakes(run, flakes)

    if is_main_run(run) and is_newest_main_run(run):
        if conclusion == "failure":
            report_red(run, failed)
        elif conclusion == "success":
            report_green(run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
