"""build (Windows): test and build this PC's release target; its installer goes to dist/.

What release.yml's app and publish jobs did, for the architecture this PC runs: the runtime and Moonshine
when third_party lacks them, scripts/build-release.ps1 -Test (tests, then the staged folder target\\dist\\Kiku)
and Inno Setup. KIKU_RELEASES in the environment (CI sets it) compiles in where updates are found and the key
they are signed with; a build without it never checks for updates.
"""
import hashlib
import os
import re
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path

import names
from common import DIST, ROOT, run, run_limited, version, windows_arch

# Inno Setup's own release (immutable on GitHub, where its download page points); the hash is the one
# GitHub records for that file.
INNO_URL = "https://github.com/jrsoftware/issrc/releases/download/is-6_7_1/innosetup-6.7.1.exe"
INNO_SHA256 = "4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0"
INNO_SECONDS = 600
RUNTIME = ("third_party/sherpa-onnx/lib/onnxruntime.lib", "third_party/moonshine-build/Release/moonshine.lib")
UPDATE_KEY = ROOT / "crates" / "core" / "update-key.txt"
STAGED = ROOT / "target" / "dist" / "Kiku"


def pwsh(script, *args):
    """Run a PowerShell script of this repository."""
    run(["pwsh", "-NoProfile", "-File", str(ROOT / script), *args])


def update_key(env):
    """The key to compile in as KIKU_UPDATE_KEY: the committed public key when KIKU_RELEASES is set, else None."""
    if not env.get("KIKU_RELEASES"):
        return None
    key = UPDATE_KEY.read_text(encoding="ascii").strip()
    if not re.fullmatch(r"[0-9a-f]{64}", key):
        sys.exit(f"{UPDATE_KEY} must hold 64 hex digits")
    return key


def iscc_candidates(env):
    """Where Inno Setup's compiler is installed, most likely first."""
    folders = [env.get("ProgramFiles(x86)"), env.get("ProgramFiles")]
    return [os.path.join(folder, "Inno Setup 6", "ISCC.exe") for folder in folders if folder]


def find_iscc(env=os.environ):
    """The path of ISCC.exe, or None."""
    return next((path for path in iscc_candidates(env) if os.path.isfile(path)), None)


def download_verified(url, sha256, path):
    """Download `url` to `path`; exit, leaving no file, unless it hashes to `sha256`."""
    digest = hashlib.sha256()
    with urllib.request.urlopen(url, timeout=120) as response, open(path, "wb") as out:
        for chunk in iter(lambda: response.read(1 << 20), b""):
            digest.update(chunk)
            out.write(chunk)
    if digest.hexdigest() != sha256:
        Path(path).unlink()
        sys.exit(f"{url} hashes to {digest.hexdigest()}, not the pinned {sha256}")


def install_inno():
    """Download the pinned Inno Setup installer, check its hash and run it silently."""
    with tempfile.TemporaryDirectory(prefix="kiku-inno-", ignore_cleanup_errors=True) as folder:
        setup = Path(folder) / "innosetup.exe"
        download_verified(INNO_URL, INNO_SHA256, setup)
        code, _ = run_limited([str(setup), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"], INNO_SECONDS, capture=False)
        if code:
            sys.exit(f"the Inno Setup installer failed with exit code {code}")


def iscc():
    """ISCC.exe, installing the pinned Inno Setup first when it is missing."""
    if not find_iscc():
        install_inno()
    path = find_iscc()
    return path or sys.exit(f"Inno Setup's ISCC.exe is not in {' or '.join(iscc_candidates(os.environ))} after installing it")


def main():
    """Build and return the exit code."""
    arch, release = windows_arch(), version()
    if not all((ROOT / path).is_file() for path in RUNTIME):
        pwsh("scripts/fetch-runtime.ps1", "-CpuOnly")
        pwsh("scripts/build-moonshine.ps1")
    env = dict(os.environ)
    if key := update_key(env):
        env["KIKU_UPDATE_KEY"] = key
    # rustup 1.28 and later install the toolchain rust-toolchain.toml pins only when asked.
    run(["rustup", "toolchain", "install"])
    run(["pwsh", "-NoProfile", "-File", str(ROOT / "scripts/build-release.ps1"), "-Test"], env=env)
    run([iscc(), "/Q", f"/DVersion={release}", f"/DArch={arch}", r"/DStaged=..\target\dist\Kiku", "installer/kiku.iss"], cwd=ROOT)
    DIST.mkdir(exist_ok=True)
    setup = ROOT / "target" / "dist" / names.installer(release, arch)
    shutil.copy2(setup, DIST)
    print(f"build: dist/{setup.name} ({setup.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
