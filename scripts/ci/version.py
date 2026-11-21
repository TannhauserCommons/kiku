"""version: print the workspace version from Cargo.toml, or `bump x.y.z` it there and in Cargo.lock.

No cargo: the release steps run where no Rust toolchain is set up, and a bump must change nothing else.
Only the workspace's own packages (the members' names) have their Cargo.lock entries rewritten.
"""
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "Cargo.toml"
LOCKFILE = ROOT / "Cargo.lock"
SEMVER = re.compile(r"\d+\.\d+\.\d+")
VERSION_LINE = re.compile(r'^version\s*=\s*"[^"]*"')


def read(path):
    """The file's text with its line endings untouched (a Windows checkout has CRLF)."""
    return path.read_text(encoding="utf-8", newline="")


def split_lines(text):
    """The lines with their endings, split on newlines only (`str.splitlines` also splits on form feeds)."""
    return re.findall(r"[^\n]*\n|[^\n]+", text)


def reversion(line, version):
    """A `version = "..."` line with a new version and the same ending and trailing comment."""
    return VERSION_LINE.sub(f'version = "{version}"', line)


def member_names(manifest):
    """The package names of the workspace members; each must take its version from the workspace."""
    names = []
    for member in tomllib.loads(manifest)["workspace"]["members"]:
        if "*" in member:
            raise SystemExit(f"version: workspace member {member} is a glob; list members by path")
        package = tomllib.loads(read(ROOT / member / "Cargo.toml"))["package"]
        if package.get("version") != {"workspace": True}:
            raise SystemExit(f"version: {member} sets its own version; use version.workspace = true")
        names.append(package["name"])
    return names


def set_in_manifest(manifest, version):
    """The manifest with `version` set in `[workspace.package]`."""
    lines, section, done = split_lines(manifest), None, False
    for i, line in enumerate(lines):
        if line.startswith("["):
            section = line.strip()
        elif section == "[workspace.package]" and VERSION_LINE.match(line):
            lines[i], done = reversion(line, version), True
            break
    if not done:
        raise SystemExit("version: no version line in [workspace.package]")
    return "".join(lines)


def set_in_lock(lock, names, version):
    """The lock file with `version` on the entries of the workspace's own packages (the ones with no source)."""
    lines = split_lines(lock)
    starts = [i for i, line in enumerate(lines) if line.startswith("[[package]]")] + [len(lines)]
    wanted, seen = set(names), set()
    for begin, end in zip(starts, starts[1:]):
        fields = {line.split(" = ", 1)[0]: i for i, line in enumerate(lines[begin:end], begin) if " = " in line and line[0] != " "}
        name = lines[fields["name"]].split('"')[1]
        if name in wanted and "source" not in fields:
            lines[fields["version"]] = reversion(lines[fields["version"]], version)
            seen.add(name)
    if seen != wanted:
        raise SystemExit(f"version: Cargo.lock has no entry for {', '.join(sorted(wanted - seen))}")
    return "".join(lines)


def current():
    """The workspace version in Cargo.toml."""
    return tomllib.loads(read(MANIFEST))["workspace"]["package"]["version"]


def bump(version):
    """Write `version` into Cargo.toml and the workspace's entries of Cargo.lock."""
    manifest = set_in_manifest(read(MANIFEST), version)
    lock = set_in_lock(read(LOCKFILE), member_names(manifest), version)
    MANIFEST.write_text(manifest, encoding="utf-8", newline="")
    LOCKFILE.write_text(lock, encoding="utf-8", newline="")


def main(argv):
    """Print the version, or with `bump x.y.z` set it."""
    if not argv:
        print(current())
        return 0
    if len(argv) != 2 or argv[0] != "bump" or not SEMVER.fullmatch(argv[1]):
        print("usage: version.py [bump x.y.z]", file=sys.stderr)
        return 2
    bump(argv[1])
    print(f"version set to {argv[1]}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
