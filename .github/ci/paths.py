"""Turn the path filter's answer into the job list `check` runs.

`.github/ci/paths.yml` has one top-level key per path-gated job id. The env var
`CHANGES` (JSON array, from dorny/paths-filter) names the keys whose paths changed.
"""

import argparse
import json
import os
import re
import sys

import ghapi

# A key at column 0, optionally quoted. Nothing else in the file matters here, so
# the file is not parsed as YAML (the scripts are standard library only).
KEY = re.compile(r"""^(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_.-]+))\s*:""")


def read_keys(path):
    """The top-level keys of the YAML file, in file order."""
    keys = []
    with open(path, encoding="utf-8") as src:
        for line in src:
            match = KEY.match(line)
            if match:
                keys.append(next(group for group in match.groups() if group))
    return keys


def read_changes(raw):
    """The JSON array of changed keys; empty or unset means none."""
    if not raw.strip():
        return []
    changes = json.loads(raw)
    if not isinstance(changes, list) or not all(isinstance(key, str) for key in changes):
        raise ValueError("CHANGES must be a JSON array of strings")
    return changes


def select(keys, changes, run_all=False):
    """`(run, skippable)`: the gated jobs to run, and the ones the gate lets skip."""
    unknown = sorted(set(changes) - set(keys))
    if unknown:
        # A filter name with no key in paths.yml means check.yml and paths.yml drifted.
        raise ValueError(f"CHANGES names keys missing from paths.yml: {', '.join(unknown)}")
    run = [key for key in keys if run_all or key in changes]
    return run, [key for key in keys if key not in run]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--all", action="store_true", help="run every gated job")
    parser.add_argument("--paths", default=".github/ci/paths.yml", help="the path map")
    args = parser.parse_args(argv)
    try:
        keys = read_keys(args.paths)
        run, skippable = select(keys, read_changes(os.environ.get("CHANGES", "")), args.all)
    except (OSError, ValueError) as err:  # json.JSONDecodeError is a ValueError
        print(f"paths: {err}", file=sys.stderr)
        return 1
    ghapi.output("run", json.dumps(run, separators=(",", ":")))
    ghapi.output("skippable", ",".join(skippable))
    return 0


if __name__ == "__main__":
    sys.exit(main())
