"""smoke (Windows): the staged folder passes the privacy gate, and the installer in dist/ installs and runs.

The install test runs the installer silently into a temp folder and runs `kiku-engine.exe --models` from
there. It changes the machine (Inno writes an uninstall entry and a Start menu shortcut under Kiku's AppId,
which would replace a real installation's), so it runs only where GITHUB_ACTIONS is set; elsewhere the
same file and engine checks run on the staged folder.
A silent install starts Kiku at its end ([Run] with Check: WizardSilent, nowait); it is stopped again here.
"""
import hashlib
import os
import sys
import tempfile
from pathlib import Path

import names
import privacy
from common import DIST, ROOT, first_model_id, lists_model, run, run_limited, version, windows_arch

STAGED = ROOT / "target" / "dist" / "Kiku"
INSTALL_SECONDS = 600
STOP_KIKU = (
    "$p = @(Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($env:KIKU_SMOKE_DIR, 'OrdinalIgnoreCase') });"
    " $p | Stop-Process -Force; $p | Wait-Process -Timeout 30"
)


def digest(path):
    """The SHA-256 of a file."""
    return hashlib.sha256(path.read_bytes()).hexdigest()


def differing(staged, installed):
    """The files of the staged folder that `installed` lacks or holds with other contents."""
    found = []
    for path in sorted(staged.rglob("*")):
        copy = installed / path.relative_to(staged)
        if path.is_file() and (not copy.is_file() or digest(copy) != digest(path)):
            found.append(str(path.relative_to(staged)))
    return found


def stop_kiku(folder):
    """End every process running from `folder` (the Kiku the installer started)."""
    run(["pwsh", "-NoProfile", "-Command", STOP_KIKU], env={**os.environ, "KIKU_SMOKE_DIR": str(folder)})


def tail(log):
    """The last lines of the installer's log."""
    return "\n".join(log.read_text(errors="replace").splitlines()[-15:]) if log.is_file() else "no setup log"


def install(setup, folder, log):
    """Run the installer silently into `folder`; exit with the log's end when it fails or takes too long."""
    command = [str(setup), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-", f"/DIR={folder}", f"/LOG={log}"]
    print("+", " ".join(command), flush=True)
    try:
        code, _ = run_limited(command, INSTALL_SECONDS, capture=False)
    except SystemExit as timeout:
        sys.exit(f"{timeout}\n{tail(log)}")
    if code:
        sys.exit(f"{setup.name} failed with exit code {code}\n{tail(log)}")


def engine_lists_models(folder):
    """Run the folder's `kiku-engine.exe --models`; True when it lists the catalog's first model."""
    models = Path(tempfile.mkdtemp(prefix="kiku-models-"))
    code, output = run_limited([str(folder / "kiku-engine.exe"), "--models"], 120, env={**os.environ, "KIKU_MODELS_DIR": str(models)})
    ok = code == 0 and lists_model(output, first_model_id())
    print(f"smoke: kiku-engine --models in {folder.name}: {'ok' if ok else 'FAILED'}")
    if not ok:
        print("\n".join(output.splitlines()[-5:]))
    return ok


def main():
    """Check this PC's build and return the exit code."""
    setup = DIST / names.installer(version(), windows_arch())
    if not setup.is_file() or not STAGED.is_dir():
        print(f"smoke: {setup} or {STAGED} is missing: run build first")
        return 1
    if privacy.gate(STAGED):
        return 1
    if os.environ.get("GITHUB_ACTIONS") != "true":
        print("smoke: not in CI, so the installer is not run; checking the staged folder instead")
        return 0 if engine_lists_models(STAGED) else 1
    with tempfile.TemporaryDirectory(prefix="kiku-smoke-", ignore_cleanup_errors=True) as work:
        folder = Path(work) / "Kiku"
        install(setup, folder, Path(work) / "setup.log")
        stop_kiku(folder)
        lacking = differing(STAGED, folder)
        if lacking:
            print(f"smoke: the install lacks or changed: {', '.join(lacking)}")
            return 1
        print(f"smoke: installed {sum(p.is_file() for p in STAGED.rglob('*'))} files, all as staged")
        return 0 if engine_lists_models(folder) else 1


if __name__ == "__main__":
    sys.exit(main())
