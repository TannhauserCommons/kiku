"""The one door to GitHub: every script calls `gh` through here, so tests stub this module."""

import json
import os
import subprocess

_repo = None
_token = False  # False: not looked up yet


def repo():
    """`owner/name` of the repository the scripts act on.

    The workflow's own repository in CI; `CI_REPO` when a checkout should look
    at another one (a private source tree whose releases live in a public
    repository); else the checkout's remote.
    """
    global _repo
    if _repo is None:
        _repo = os.environ.get("GITHUB_REPOSITORY") or os.environ.get("CI_REPO") or gh(
            "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner"
        ).strip()
    return _repo


def project_token():
    """The token git uses for this repo's remote, or None to leave gh's own login.

    `gh` has one active account; git picks a credential per remote. On a
    machine with several GitHub accounts this makes `gh` act as the account the
    project's git config names, with no global switch. CI sets GH_TOKEN itself.
    """
    global _token
    if _token is not False:
        return _token
    _token = None
    if os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN"):
        return None
    try:
        url = subprocess.run(
            ["git", "remote", "get-url", "origin"], capture_output=True, text=True, timeout=10
        ).stdout.strip()
        if not url.startswith("https://github.com/"):
            return None
        # Never prompt: with no stored credential, gh's own login is the answer.
        quiet = {**os.environ, "GIT_TERMINAL_PROMPT": "0", "GCM_INTERACTIVE": "never"}
        filled = subprocess.run(
            ["git", "credential", "fill"], input=f"url={url}\n\n",
            capture_output=True, text=True, timeout=10, env=quiet,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    for line in filled.stdout.splitlines() if filled.returncode == 0 else []:
        if line.startswith("password="):
            _token = line.removeprefix("password=") or None
    return _token


def gh(*args, stdin=None, check=True):
    """Run `gh` and return its stdout. A failure raises with gh's own message."""
    token = project_token()
    env = {**os.environ, "GH_TOKEN": token} if token else None
    done = subprocess.run(
        ["gh", *args], input=stdin, capture_output=True, text=True, encoding="utf-8", env=env
    )
    if check and done.returncode != 0:
        raise RuntimeError(f"gh {' '.join(args)}: {done.stderr.strip() or done.stdout.strip()}")
    return done.stdout


def api(path, method="GET", body=None, paginate=False):
    """A REST call. `path` is relative to `repos/{repo}/` unless it starts with `/`.

    With `paginate`, list endpoints come back as one list and object endpoints
    that wrap a list (`{"total_count": .., "jobs": [..]}`) as a list of pages.
    """
    url = path.lstrip("/") if path.startswith("/") else f"repos/{repo()}/{path}"
    args = ["api", "--method", method, url]
    if paginate:
        args += ["--paginate", "--slurp"]
    if body is not None:
        args += ["--input", "-"]
    out = gh(*args, stdin=None if body is None else json.dumps(body))
    if not out.strip():
        return None
    data = json.loads(out)
    if paginate and data and all(isinstance(page, list) for page in data):
        return [item for page in data for item in page]
    return data


def ensure_label(name, color="d73a4a", description=""):
    """Create the label if the repository lacks it."""
    labels = api("labels?per_page=100", paginate=True) or []
    if not any(label["name"] == name for label in labels):
        api("labels", "POST", {"name": name, "color": color, "description": description})


def find_issue(label, title=None):
    """The open issue with that label (and exact title), or None."""
    issues = api(f"issues?state=open&labels={label}&per_page=100", paginate=True) or []
    for issue in issues:
        if "pull_request" not in issue and (title is None or issue["title"] == title):
            return issue
    return None


def upsert_issue(label, title, body, comment=None):
    """One issue per (label, title): create it, or comment on the open one.

    Returns `(number, created)`.
    """
    issue = find_issue(label, title)
    if issue:
        api(f"issues/{issue['number']}/comments", "POST", {"body": comment or body})
        return issue["number"], False
    ensure_label(label)
    made = api("issues", "POST", {"title": title, "body": body, "labels": [label]})
    return made["number"], True


def close_issue(number, comment):
    api(f"issues/{number}/comments", "POST", {"body": comment})
    api(f"issues/{number}", "PATCH", {"state": "closed", "state_reason": "completed"})


def output(name, value):
    """A step output: appended to GITHUB_OUTPUT in CI, always printed."""
    print(f"{name}={value}")
    path = os.environ.get("GITHUB_OUTPUT")
    if path:
        with open(path, "a", encoding="utf-8") as out:
            out.write(f"{name}={value}\n")
