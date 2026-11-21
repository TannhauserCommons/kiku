"""Feature switches: validate `switches.toml`, list them, and report on them.

Contract: docs/context/contract.md, "switches.py".
"""

import argparse
import datetime
import os
import re
import shutil
import subprocess
import sys
import tomllib

import ghapi

REQUIRED = ("name", "what", "owner", "area", "default", "ready", "expires")
TOP_KEYS = ("cap_per_area", "tombstones", "switch")
NAME = re.compile(r"^[a-z0-9-]+$")
VERSION = re.compile(r"^v?(\d+)\.(\d+)\.(\d+)$")
DATE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
LABEL = "switch-removal"


def load(path):
    """The parsed file; a missing file reads as empty."""
    try:
        with open(path, "rb") as src:
            return tomllib.load(src)
    except FileNotFoundError:
        return {}


def switches_of(data):
    """The switch tables that have a name, tolerating a malformed file (`check` reports that)."""
    items = data.get("switch", [])
    if not isinstance(items, list):
        return []
    return [i for i in items if isinstance(i, dict) and isinstance(i.get("name"), str)]


def current_version():
    """The version `main` carries: the last non-empty stdout line of `mise run version`."""
    # which() resolves mise.exe or a mise.cmd shim on Windows, where a bare name may not run.
    mise = shutil.which("mise")
    if not mise:
        raise RuntimeError("mise not found; pass --version")
    done = subprocess.run([mise, "run", "version"], capture_output=True, text=True)
    lines = [line.strip() for line in done.stdout.splitlines() if line.strip()]
    if done.returncode != 0 or not lines:
        raise RuntimeError(f"`mise run version` failed: {done.stderr.strip() or 'no output'}")
    return lines[-1]


def commit_date():
    """HEAD's commit date, so the same commit always gets the same answer."""
    done = subprocess.run(["git", "log", "-1", "--format=%cs"], capture_output=True, text=True)
    if done.returncode != 0 or not done.stdout.strip():
        raise RuntimeError(f"`git log` failed: {done.stderr.strip() or 'no commits'}")
    return done.stdout.strip()


def version_tuple(text):
    match = VERSION.match(text.strip())
    return tuple(int(part) for part in match.groups()) if match else None


def validate(data, get_version, get_today):
    """Every problem in the parsed file as `(name or "file", message)`.

    `get_version` and `get_today` are called only when a switch needs them, so a
    file with no expiry of that kind never runs `mise` or `git`.
    """
    found = []
    for key in data:
        if key not in TOP_KEYS:
            found.append(("file", f"unknown key `{key}`"))
    cap = data.get("cap_per_area")
    if "cap_per_area" not in data:
        found.append(("file", "missing cap_per_area"))
    elif not isinstance(cap, int) or isinstance(cap, bool) or cap < 1:
        found.append(("file", "cap_per_area must be a positive integer"))
        cap = None
    tombstones = data.get("tombstones", [])
    if not isinstance(tombstones, list) or not all(isinstance(t, str) for t in tombstones):
        found.append(("file", "tombstones must be a list of names"))
        tombstones = []
    items = data.get("switch", [])
    if not isinstance(items, list) or not all(isinstance(i, dict) for i in items):
        found.append(("file", "switch must be a list of [[switch]] tables"))
        items = []

    seen, per_area, expiries = set(), {}, []
    for number, item in enumerate(items, 1):
        name = item.get("name") if isinstance(item.get("name"), str) and item.get("name") else f"switch #{number}"
        for key in REQUIRED:
            if key not in item:
                found.append((name, f"missing {key}"))
        for key in item:
            if key not in REQUIRED:
                found.append((name, f"unknown key `{key}`"))
        if "name" in item:
            if not isinstance(item["name"], str) or not NAME.match(item["name"]):
                found.append((name, "name must match [a-z0-9-]+"))
            elif item["name"] in seen:
                found.append((name, "duplicate name"))
            elif item["name"] in tombstones:
                found.append((name, "name is a tombstone: removed switches are never reused"))
            seen.add(item["name"] if isinstance(item["name"], str) else None)
        for key in ("what", "owner", "area"):
            if key in item and (not isinstance(item[key], str) or not item[key].strip()):
                found.append((name, f"{key} must be a non-empty string"))
        if isinstance(item.get("area"), str) and item["area"].strip():
            per_area[item["area"]] = per_area.get(item["area"], 0) + 1
        if "default" in item and item["default"] not in ("off", "on"):
            found.append((name, 'default must be "off" or "on"'))
        if "ready" in item and not isinstance(item["ready"], bool):
            found.append((name, "ready must be true or false"))
        elif item.get("ready") is True and item.get("default") == "on":
            found.append((name, "ready is true but default is already on"))
        if "expires" in item:
            expires = item["expires"]
            if isinstance(expires, str) and VERSION.match(expires) and not expires.startswith("v"):
                expiries.append((name, "version", expires))
            elif isinstance(expires, str) and DATE.match(expires) and _is_date(expires):
                expiries.append((name, "date", expires))
            else:
                found.append((name, "expires must be a release x.y.z or a date YYYY-MM-DD"))

    if cap is not None:
        for area, count in sorted(per_area.items()):
            if count > cap:
                found.append(("file", f"area {area} has {count} switches, the cap is {cap}"))
    found += _expired(expiries, get_version, get_today)
    return found


def _is_date(text):
    try:
        datetime.date.fromisoformat(text)
    except ValueError:
        return False
    return True


def _expired(expiries, get_version, get_today):
    """Findings for the version and date expiries that are past."""
    found = []
    versions = [e for e in expiries if e[1] == "version"]
    dates = [e for e in expiries if e[1] == "date"]
    if versions:
        try:
            now = get_version()
            have = version_tuple(now)
            if have is None:
                raise RuntimeError(f"current version `{now}` is not x.y.z")
        except RuntimeError as err:
            found.append(("file", str(err)))
        else:
            for name, _, expires in versions:
                if have >= version_tuple(expires):
                    found.append((name, f"expired: the version is {now}, the switch expired at {expires}"))
    if dates:
        try:
            today = get_today()
            if not DATE.match(today) or not _is_date(today):
                raise RuntimeError(f"today `{today}` is not YYYY-MM-DD")
        except RuntimeError as err:
            found.append(("file", str(err)))
        else:
            for name, _, expires in dates:
                if today > expires:
                    found.append((name, f"expired: it is {today}, the switch expired on {expires}"))
    return found


def cmd_check(args):
    if not os.path.exists(args.file):
        # A project may have no switches yet; the cap has nothing to cap.
        print("switches: ok (0 switches)")
        return 0
    data = load(args.file)
    get_version = (lambda: args.version) if args.version else current_version
    get_today = (lambda: args.today) if args.today else commit_date
    findings = validate(data, get_version, get_today)
    for name, message in findings:
        print(f"{args.file}: {name}: {message}")
    if findings:
        return 1
    print(f"switches: ok ({len(switches_of(data))} switches)")
    return 0


def cmd_list(args):
    for item in switches_of(load(args.file)):
        if args.off and item.get("default") != "off":
            continue
        if args.on and item.get("default") != "on":
            continue
        if args.ready and item.get("ready") is not True:
            continue
        print(item.get("name"))
    return 0


def cmd_notes(args):
    """Markdown for the release draft; nothing at all when there is nothing to say."""
    items = switches_of(load(args.file))
    on = [i for i in items if i.get("default") == "on"]
    ready = [i for i in items if i.get("ready") is True and i.get("default") != "on"]
    if not on and not ready:
        return 0
    print("### Switches")
    for title, group in (("On in this release:", on), ("Ready to turn on:", ready)):
        if group:
            print(f"\n{title}")
            for item in group:
                print(f"- `{item['name']}`: {item.get('what', '')}")
    return 0


def cmd_shipped(args):
    """One removal issue per switch that is on, opened once and commented on after."""
    for item in switches_of(load(args.file)):
        if item.get("default") != "on":
            continue
        name = item["name"]
        body = (
            f"The switch `{name}` ({item.get('what', '')}) is on in this release.\n\n"
            "Remove the switch and its off path before the next release, "
            "so the code has one behaviour again."
        )
        number, created = ghapi.upsert_issue(
            label=LABEL,
            title=f"Remove switch {name}",
            body=body,
            comment=f"`{name}` is still on in this release; remove it before the next one.",
        )
        print(f"{name}: {'opened' if created else 'commented on'} #{number}")
    return 0


def main(argv=None):
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--file", default="switches.toml", help="default: switches.toml")
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)

    check = sub.add_parser("check", parents=[common], help="validate the file")
    check.add_argument("--version", help="the version main carries (default: mise run version)")
    check.add_argument("--today", help="YYYY-MM-DD instead of HEAD's commit date")
    check.set_defaults(run=cmd_check)

    listing = sub.add_parser("list", parents=[common], help="switch names, one per line")
    group = listing.add_mutually_exclusive_group()
    group.add_argument("--off", action="store_true", help="only switches that default to off")
    group.add_argument("--on", action="store_true", help="only switches that default to on")
    group.add_argument("--ready", action="store_true", help="only switches marked ready")
    listing.set_defaults(run=cmd_list)

    sub.add_parser("notes", parents=[common], help="release notes section").set_defaults(run=cmd_notes)
    sub.add_parser("shipped", parents=[common], help="file removal issues for on switches").set_defaults(
        run=cmd_shipped
    )

    args = parser.parse_args(argv)
    try:
        return args.run(args)
    except tomllib.TOMLDecodeError as err:
        print(f"{args.file}: file: invalid TOML: {err}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
