"""What an agent runs first: red `main`, red PRs, stuck PRs, the candidate, open flakes.

Run as `python .github/ci/status.py`. It reports and never gates, so it exits 0
even when GitHub cannot be reached (it then prints one `status: <message>` line).
"""

import argparse
import os
import sys
import unicodedata
from datetime import datetime, timezone

import ghapi

CANDIDATE_MARKER = "<!-- ci:candidate -->"
STUCK_AFTER_SECONDS = 3600
# Gate conclusions that mean "it ran and said no", as opposed to "not green yet".
GATE_FAILED = {"failure", "timed_out"}


def branch():
    """The release branch: env `CI_BRANCH`, else `main`. Empty counts as unset."""
    return os.environ.get("CI_BRANCH", "").strip() or "main"


def clean(text, limit=80):
    """Strip control characters and cap the length of text that came from GitHub."""
    kept = "".join(ch for ch in str(text) if unicodedata.category(ch) not in ("Cc", "Cf", "Zl", "Zp"))
    return kept.strip()[:limit]


def parse_time(stamp):
    """An API timestamp (`2026-01-02T03:04:05Z`) as an aware datetime."""
    return datetime.fromisoformat(stamp)


def age_text(seconds):
    """`2h 5m`, or `7m` below an hour."""
    minutes = int(seconds // 60)
    return f"{minutes // 60}h {minutes % 60}m" if minutes >= 60 else f"{minutes}m"


def main_line():
    """`main: red  <where to look>` or `main: green`, named after the release branch.

    The issue is always labelled `main-red`, whatever the branch is called. Red
    while that issue is open, and also when the newest finished `main.yml` run
    on the branch did not succeed: a run that fails at startup (a broken workflow
    file) never reaches `after`, so no issue exists for it.
    """
    name = branch()
    issue = ghapi.find_issue("main-red")
    if issue:
        return f"{name}: red  {issue['html_url']}"
    runs = ghapi.api(f"actions/workflows/main.yml/runs?branch={name}&status=completed&per_page=10")
    for run in (runs or {}).get("workflow_runs", []):
        # Cancelled is neither red nor green: the run before it still stands.
        if run.get("conclusion") in ("cancelled", "skipped"):
            continue
        if run.get("conclusion") != "success":
            return f"{name}: red  {run['conclusion']}  {run['html_url']}"
        break
    return f"{name}: green"


def gate_run(sha):
    """The newest check-run named `gate` on a commit, or None."""
    data = ghapi.api(f"commits/{sha}/check-runs?check_name=gate&per_page=100")
    runs = [r for r in (data or {}).get("check_runs", []) if r.get("name") == "gate"]
    return max(runs, key=lambda r: r.get("started_at") or "", default=None)


def pr_lines(now):
    """One line per open PR that needs someone: failed, then stuck, then green.

    Every PR merges only on the owner's yes, nothing auto-merges, so a green PR
    waits on a person. Listing it keeps none sitting unnoticed. The order puts
    what an agent must act on first. A green draft is not waiting for the
    owner, so it prints nothing.

    A failed gate is reported as failed only, not again as stuck. The push time
    is the PR's `updated_at`: the pulls list has no push time, and asking each
    commit for one costs a call per PR. A comment or label bumps `updated_at`,
    so this can only under-report stuck PRs, never invent one.
    """
    red, stuck, waiting = [], [], []
    for pr in ghapi.api("pulls?state=open&per_page=100", paginate=True) or []:
        gate = gate_run(pr["head"]["sha"])
        conclusion = gate.get("conclusion") if gate else None
        title = clean(pr["title"])
        if conclusion in GATE_FAILED:
            red.append(f"pr #{pr['number']}: gate failed  {title}")
        elif conclusion == "success":
            if not pr.get("draft"):
                waiting.append(f"pr #{pr['number']}: green, waiting for the owner  {title}")
        else:
            idle = (now - parse_time(pr["updated_at"])).total_seconds()
            if idle > STUCK_AFTER_SECONDS:
                state = "missing" if gate is None else (conclusion or gate.get("status") or "unknown")
                stuck.append(f"pr #{pr['number']}: stuck  gate {state} for {age_text(idle)}  {title}")
    return red + stuck + waiting


def candidate_line():
    """`candidate: <tag> <short sha>` from the standing draft release, or `candidate: none`."""
    for release in ghapi.api("releases?per_page=30") or []:
        if release.get("draft") and (release.get("body") or "").rstrip().endswith(CANDIDATE_MARKER):
            return f"candidate: {release['tag_name']} {str(release.get('target_commitish', ''))[:7]}"
    return "candidate: none"


def flaky_line():
    """`flaky: <n> open`, or None when there are none."""
    issues = ghapi.api("issues?state=open&labels=flaky&per_page=100", paginate=True) or []
    count = sum(1 for issue in issues if "pull_request" not in issue)
    return f"flaky: {count} open" if count else None


def report(now):
    """The status lines, in the contract's order, only those that apply."""
    lines = [main_line(), *pr_lines(now), candidate_line(), flaky_line()]
    return [line for line in lines if line]


def main(argv=None):
    """Print the status. Always exit 0: a status that cannot be read is not a gate."""
    argparse.ArgumentParser(description=__doc__.splitlines()[0]).parse_args(argv)
    try:
        lines = report(datetime.now(timezone.utc))
    except (RuntimeError, OSError, ValueError) as err:
        # gh's own message (not found, not logged in, no repo) is the useful part.
        message = str(err).strip().splitlines()
        print(f"status: {message[0] if message else type(err).__name__}")
        return 0
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
