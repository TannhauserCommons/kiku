"""The skeleton's own workflow rules, on top of actionlint.

Standard library only, so there is no YAML parser: workflows are read by
indentation, the way GitHub workflows are written (`jobs:` at column 0, job ids
one level in, job keys one level further, steps as `- ` list items).
"""

import argparse
import glob
import re
import sys

SHA_PIN = re.compile(r"^[\w.-]+/[\w.-]+(/[\w./-]+)?@[0-9a-f]{40}$")
KEY = re.compile(r"""^(?:"([^"]+)"|'([^']+)'|([A-Za-z0-9_.-]+))\s*:(?:\s+(.*)|)$""")
BLOCK = re.compile(r"^[|>][+-]?\d?[+-]?$")
# What would make a `mise run` line more than one command.
CHAINED = re.compile(r"[;&|`<>]|\$\(")
MISE_RUN = re.compile(r"^mise run (\S+)")
# `${{ a && b || c }}` is one value to the shell, whatever operators it holds.
EXPRESSION = re.compile(r"\$\{\{.*?\}\}")
EXPRESSION_BODY = re.compile(r"\$\{\{(.*?)\}\}")
PULL_REQUEST_TARGET = re.compile(r"\bpull_request_target\b")
# `false`, or `github.ref == 'refs/heads/<branch>'` comparisons joined by `||`: the
# release branch differs per project, and a cache must only be saved from there.
_REF_IS_BRANCH = r"github\.ref\s*==\s*'refs/heads/[A-Za-z0-9._/-]+'"
CACHE_SAVE_EXPRESSION = re.compile(
    rf"\$\{{\{{\s*{_REF_IS_BRANCH}(?:\s*\|\|\s*{_REF_IS_BRANCH})*\s*\}}\}}"
)
CACHE_SAVE_MESSAGE = (
    "jdx/mise-action must set cache_save to false or to a comparison of github.ref "
    "with the release branch (only the release branch saves caches)"
)
STRING = re.compile(r"'(?:[^']|'')*'")
CALL = re.compile(r"([A-Za-z_]\w*)\s*\(")
OPERATORS = re.compile(r"&&|\|\||!=|==|[!()]")
LITERALS = ("true", "false", "null")
# The only contexts a `run:` line may read: ones no pull request author controls.
TRUSTED = tuple(
    re.compile(pattern)
    for pattern in (
        r"inputs\.[\w-]+",
        r"steps\.[\w-]+\.outputs\.[\w-]+",
        r"matrix\.[\w-]+",
        r"github\.(?:sha|run_id)",
        r"github\.event\.before",
        r"github\.event\.pull_request\.base\.sha",
        r"github\.event\.workflow_run\.id",
    )
)


def strip_comment(line):
    """The line without a trailing `# comment`; a `#` inside quotes is text."""
    quote = None
    for i, char in enumerate(line):
        if quote:
            if char == quote:
                quote = None
        elif char in "\"'":
            quote = char
        elif char == "#" and (i == 0 or line[i - 1].isspace()):
            return line[:i]
    return line


def indent_of(line):
    return len(line) - len(line.lstrip(" "))


def content_of(line):
    """The line's text without indentation or comment; empty for blank and comment lines."""
    return strip_comment(line).strip()


def unquote(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def parse_key(content):
    """`(key, value)` of a `key: value` line, or None. The value is unquoted."""
    match = KEY.match(content)
    if not match:
        return None
    key = next(group for group in match.groups()[:3] if group)
    return key, unquote(match.group(4) or "")


def read_keys(lines, start, end, key_indent, scalars):
    """The `key: value` pairs at exactly `key_indent` in `lines[start:end]`.

    Returns `{key: (line index, value)}`. For the `scalars` keys a value that
    is empty (the value sits on the next line) or a block scalar (`|`, `>`) is
    read from the more indented lines below; a block's value is its non-blank
    lines joined by newlines, so one command is one line.
    """
    found = {}
    i = start
    while i < end:
        parsed = parse_key(content_of(lines[i])) if indent_of(lines[i]) == key_indent else None
        if not parsed:
            i += 1
            continue
        (key, value), at = parsed, i
        i += 1
        if key in scalars and (value == "" or BLOCK.match(value)):
            is_block = value != ""
            body = []
            while i < end and (not lines[i].strip() or indent_of(lines[i]) > key_indent):
                # A block's body is script text: a `#` there is not a YAML comment.
                body.append((lines[i] if is_block else strip_comment(lines[i])).strip())
                i += 1
            body = [text for text in body if text]
            if is_block:
                value = "\n".join(body)
            elif body:
                value = unquote(body[0])
        found[key] = (at, value)
    return found


def section_end(lines, start, key_indent, allow_dash=False):
    """Index after the block that begins at `start`, whose parent key is at `key_indent`."""
    for i in range(start, len(lines)):
        content = content_of(lines[i])
        if not content:
            continue
        indent = indent_of(lines[i])
        if indent < key_indent or (indent == key_indent and not (allow_dash and content[0] == "-")):
            return i
    return len(lines)


def check_pin(value):
    """A message when `uses:` is not a SHA pin (local paths are fine), else None."""
    if value.startswith("./") or SHA_PIN.match(value):
        return None
    return f"uses `{value}` is not pinned to a 40-hex commit SHA"


def untrusted(expression):
    """What an `${{ }}` body reads that a `run:` line may not: contexts, calls, stray tokens."""
    expression = STRING.sub(" ", expression)
    calls = [f"{name}()" for name in CALL.findall(expression)]
    if calls:
        return calls
    tokens = OPERATORS.sub(" ", expression).split()
    return [t for t in tokens if t not in LITERALS and not any(p.fullmatch(t) for p in TRUSTED)]


def inline_value(flow, key):
    """The value of `key` in a one-line `{a: 1, b: 2}` mapping, unquoted, or None."""
    match = re.search(rf"[{{,]\s*[\"']?{key}[\"']?\s*:\s*", flow)
    if not match:
        return None
    rest, i, quote = flow[match.end() :], 0, None
    while i < len(rest):
        if quote:
            quote = None if rest[i] == quote else quote
        elif rest.startswith("${{", i):
            # An expression holds `}` and quotes of its own; skip to its end.
            i = rest.find("}}", i) + 1 if "}}" in rest[i:] else len(rest)
        elif rest[i] in "\"'":
            quote = rest[i]
        elif rest[i] in ",}":
            break
        i += 1
    return unquote(rest[:i])


def cache_save_value(sub, keys, key_indent):
    """The `cache_save` value in the step's own `with:` mapping, or None.

    Only the lines that belong to `with:` count: those indented deeper than the
    `with:` key, up to the next key at its indent or shallower (a sibling `env:`
    may hold a `cache_save` of its own).
    """
    if "with" not in keys:
        return None
    at, value = keys["with"]
    if value:
        return inline_value(value, "cache_save")
    end = next((i for i in range(at + 1, len(sub)) if content_of(sub[i]) and indent_of(sub[i]) <= key_indent), len(sub))
    first = next((i for i in range(at + 1, end) if content_of(sub[i])), None)
    if first is None:
        return None
    return read_keys(sub, at + 1, end, indent_of(sub[first]), ()).get("cache_save", (0, None))[1]


def cache_save_ok(value):
    """Whether a `cache_save` value can only save from the release branch."""
    return value is not None and (value == "false" or CACHE_SAVE_EXPRESSION.fullmatch(value) is not None)


def check_step(sub, key_indent):
    """Findings `(offset in sub, message)` for one step whose keys sit at `key_indent`."""
    keys = read_keys(sub, 0, len(sub), key_indent, ("uses", "run", "name"))
    found = []
    if "uses" in keys:
        message = check_pin(keys["uses"][1])
        if message:
            found.append((keys["uses"][0], message))
        # Only main may save caches and the action's default is to save.
        is_mise = keys["uses"][1].lower().split("@")[0] == "jdx/mise-action"
        if is_mise and not cache_save_ok(cache_save_value(sub, keys, key_indent)):
            found.append((keys["uses"][0], CACHE_SAVE_MESSAGE))
    if "run" in keys:
        at, value = keys["run"]
        # Expressions are filled in before the shell sees the line, so one that
        # reads attacker-controlled text is a shell injection.
        seen = []
        for body in EXPRESSION_BODY.findall(value):
            for token in untrusted(body):
                if token not in seen:
                    seen.append(token)
                    found.append((at, f"run: reads ${{{{ {token} }}}}; pass it through env instead"))
        task = MISE_RUN.match(value)
        if "\n" in value or not task or CHAINED.search(EXPRESSION.sub("", value)):
            found.append((at, "run is not a single `mise run ...` line"))
        else:
            # The arguments stay out of the name: after.py matches on `mise run <task>`.
            want = f"mise run {task.group(1)}"
            if "name" not in keys:
                found.append((at, f"step has no name; it must be exactly `{want}`"))
            elif keys["name"][1] != want:
                found.append((keys["name"][0], f"step name `{keys['name'][1]}` must be exactly `{want}`"))
    return found


def check_steps(lines, start, end):
    """Findings `(line index, message)` for the `- ` items in `lines[start:end]`."""
    items = [i for i in range(start, end) if content_of(lines[i]).startswith("-")]
    if not items:
        return []
    item_indent = indent_of(lines[items[0]])
    items = [i for i in items if indent_of(lines[i]) == item_indent]
    found = []
    for n, first in enumerate(items):
        last = items[n + 1] if n + 1 < len(items) else end
        sub = lines[first:last]
        # Turn the `- ` into spaces so the first key lines up with the others.
        dash = sub[0].lstrip(" ")
        rest = dash[1:].lstrip(" ")
        column = indent_of(sub[0]) + len(dash) - len(rest)
        sub[0] = " " * column + rest
        if not rest:
            nxt = next((l for l in sub[1:] if content_of(l)), None)
            if nxt is None:
                continue
            column = indent_of(nxt)
        found += [(first + at, message) for at, message in check_step(sub, column)]
    return found


def check_job(lines, start, end):
    """Findings `(line index, message)` for the job whose id line is `lines[start]`."""
    first = next((i for i in range(start + 1, end) if content_of(lines[i])), None)
    if first is None:
        return []
    key_indent = indent_of(lines[first])
    keys = read_keys(lines, start + 1, end, key_indent, ("uses",))
    found = []
    if "uses" in keys:
        # A job that calls a reusable workflow cannot set timeout-minutes at all.
        message = check_pin(keys["uses"][1])
        if message:
            found.append((keys["uses"][0], message))
    elif "timeout-minutes" not in keys:
        found.append((start, "job has no timeout-minutes"))
    if "steps" in keys:
        steps = keys["steps"][0] + 1
        found += check_steps(lines, steps, section_end(lines, steps, key_indent, allow_dash=True))
    return found


def read_only(lines, at):
    """Whether the top-level `permissions:` at `lines[at]` is exactly `contents: read`.

    Jobs widen what they need themselves, so only the workflow-wide default is held to this.
    """
    value = parse_key(content_of(lines[at]))[1]
    if value:
        inline = value.startswith("{") and value.endswith("}")
        grants = [part.strip() for part in value[1:-1].split(",")] if inline else [value]
    else:
        grants = [content_of(line) for line in lines[at + 1 : section_end(lines, at + 1, 0)] if content_of(line)]
    return grants == ["contents: read"]


def lint_text(text):
    """All findings for one workflow, as sorted `(line number, message)`."""
    lines = text.splitlines()
    found = []
    top = {}
    for i, line in enumerate(lines):
        parsed = parse_key(content_of(line)) if indent_of(line) == 0 else None
        if parsed:
            top.setdefault(parsed[0], i)
        if PULL_REQUEST_TARGET.search(strip_comment(line)):
            found.append((i, "pull_request_target is not allowed"))
    if "permissions" not in top:
        found.append((0, "no top-level permissions:"))
    elif not read_only(lines, top["permissions"]):
        found.append((top["permissions"], "top-level permissions must be exactly `contents: read`"))
    if "jobs" in top:
        start = top["jobs"] + 1
        end = section_end(lines, start, 0)
        body = [i for i in range(start, end) if content_of(lines[i])]
        if body:
            job_indent = indent_of(lines[body[0]])
            jobs = [i for i in body if indent_of(lines[i]) == job_indent and parse_key(content_of(lines[i]))]
            for n, job in enumerate(jobs):
                found += check_job(lines, job, jobs[n + 1] if n + 1 < len(jobs) else end)
    return sorted((i + 1, message) for i, message in found)


def lint_file(path):
    """Findings for one file as `file:line: message` strings."""
    shown = path.replace("\\", "/")
    try:
        with open(path, encoding="utf-8") as src:
            text = src.read()
    except OSError as err:
        return [f"{shown}:0: cannot read: {err}"]
    return [f"{shown}:{line}: {message}" for line, message in lint_text(text)]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("files", nargs="*", help="default: .github/workflows/*.yml")
    args = parser.parse_args(argv)
    files = args.files or sorted(
        glob.glob(".github/workflows/*.yml") + glob.glob(".github/workflows/*.yaml")
    )
    findings = [finding for path in files for finding in lint_file(path)]
    for finding in findings:
        print(finding)
    if findings:
        return 1
    print(f"lint: ok ({len(files)} files)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
