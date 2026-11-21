"""docker_image: run a script in the Linux build image (scripts/linux-build.Dockerfile).

On Linux the repository is mounted and third_party, target and cargo's registry (target/cargo-registry) stay
in it, so CI can cache them; what the container made is handed back to the caller's user. On Windows it runs
through Docker Desktop with those three folders in Docker volumes, the ones scripts/linux-gate.ps1 uses, so
the Windows build is never touched.
"""
import os
import subprocess
import sys

from common import ROOT

IMAGE = "kiku-linux-build"
VOLUMES = {"kiku-linux-third-party": "/src/third_party", "kiku-linux-target": "/src/target", "kiku-linux-cargo": "/opt/cargo/registry"}


def script(*commands, owned=("third_party", "target")):
    """A bash script that makes sure the runtime is there, runs `commands` and hands `owned` back to the caller."""
    return "\n".join([
        "set -e",
        f"if [ -n \"$OWNER\" ]; then trap 'chown -R \"$OWNER\" {' '.join(owned)}' EXIT; fi",
        "if [ ! -e third_party/sherpa-onnx/lib/libonnxruntime.so ]; then ./scripts/fetch-runtime.sh; fi",
        "if [ ! -e third_party/moonshine-build/libmoonshine.a ]; then ./scripts/build-moonshine.sh; fi",
        *commands,
    ])


def mounts():
    """The `docker run` arguments that put the repository, and the folders that outlive a run, in the container."""
    args = ["-v", f"{ROOT}:/src"]
    if sys.platform == "win32":
        return args + [arg for volume, path in VOLUMES.items() for arg in ("-v", f"{volume}:{path}")]
    registry = ROOT / "target" / "cargo-registry"
    registry.mkdir(parents=True, exist_ok=True)
    return args + ["-v", f"{registry}:/opt/cargo/registry", "-e", f"OWNER={os.getuid()}:{os.getgid()}"]


def run_script(text):
    """Build the image, run the bash script in it, and return the first non-zero exit code."""
    build = ["docker", "build", "-q", "-t", IMAGE, "-f", str(ROOT / "scripts" / "linux-build.Dockerfile"), str(ROOT / "scripts")]
    try:
        code = subprocess.run(build).returncode
        return code or subprocess.run(["docker", "run", "--rm", *mounts(), IMAGE, "bash", "-c", text]).returncode
    except FileNotFoundError:
        print("docker is not installed", file=sys.stderr)
        return 1
