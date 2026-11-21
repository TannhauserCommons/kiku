"""common: what the build, smoke and publish scripts share: paths, the version, this PC's architecture, running commands."""
import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / "dist"
CATALOG = ROOT / "crates" / "core" / "catalog.json"


def version():
    """The workspace version in Cargo.toml."""
    return tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["package"]["version"]


def windows_arch(env=os.environ):
    """`x64` or `arm64`: the architecture of Windows itself, which an emulated x64 Python on ARM64 still reports."""
    machine = (env.get("PROCESSOR_ARCHITEW6432") or env.get("PROCESSOR_ARCHITECTURE") or "").upper()
    if machine not in ("AMD64", "ARM64"):
        sys.exit(f"unknown Windows architecture {machine!r}")
    return "x64" if machine == "AMD64" else "arm64"


def first_model_id():
    """The id of the catalog's first model, which `kiku-engine --models` lists first."""
    return json.loads(CATALOG.read_text(encoding="utf-8"))["models"][0]["id"]


def lists_model(output, model_id):
    """Whether a line of `kiku-engine --models` output starts with the model's id."""
    return any(line.split()[:1] == [model_id] for line in output.splitlines())


def run(command, **kwargs):
    """Run a command, echoing it first; exit with a message when it fails."""
    print("+", " ".join(str(part) for part in command), flush=True)
    code = subprocess.run(command, **kwargs).returncode
    if code:
        sys.exit(f"{Path(str(command[0])).name} failed with exit code {code}")


def run_limited(command, seconds, capture=True, **kwargs):
    """Run a command that must end within `seconds`; returns `(exit code, output)`, or exits when it does not.

    A timeout ends the whole process tree (an installer's setup runs in a child of its own). With
    `capture=False` the command's output goes nowhere and the returned output is empty: a program that
    starts a long-running child (an installer that starts the app) would otherwise hand it the pipe's write
    end, and reading the pipe would not end until the child did. Stdin is never inherited.
    """
    out = subprocess.PIPE if capture else subprocess.DEVNULL
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=out, stderr=subprocess.STDOUT if capture else subprocess.DEVNULL,
                               text=True, close_fds=True, **kwargs)
    try:
        output, _ = process.communicate(timeout=seconds)
    except subprocess.TimeoutExpired:
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(process.pid)], capture_output=True)
        else:
            process.kill()
        process.communicate()
        sys.exit(f"{Path(str(command[0])).name} did not finish in {seconds} s")
    return process.returncode, output or ""
