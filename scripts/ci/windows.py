"""windows: the workspace tests on Windows, and the settings snapshots when KIKU_SNAPSHOTS names a folder.

Fetches the runtime and builds Moonshine when third_party lacks them (build_windows.py says which files).
"""
import os
import sys

import build_windows
from common import ROOT, run


def main(env=os.environ):
    """Run the tests (and the snapshots) and return the exit code."""
    if not all((ROOT / path).is_file() for path in build_windows.RUNTIME):
        build_windows.pwsh("scripts/fetch-runtime.ps1", "-CpuOnly")
        build_windows.pwsh("scripts/build-moonshine.ps1")
    # rustup 1.28 and later install the toolchain rust-toolchain.toml pins only when asked.
    run(["rustup", "toolchain", "install"])
    run(["cargo", "test", "--release", "--locked", "--workspace", "--tests"], cwd=ROOT)
    if env.get("KIKU_SNAPSHOTS"):
        run(["cargo", "test", "--release", "--locked", "-p", "kiku", "snapshots", "--", "--ignored"], cwd=ROOT)
    return 0


if __name__ == "__main__":
    sys.exit(main())
