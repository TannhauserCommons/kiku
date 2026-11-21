"""build (Linux): test and pack the engine in the build image; its tarball and the release's "Downloads" text go to dist/.

The same steps release.yml's linux job ran. NOTES.md is written here because one job has to, and the
Linux job is it; candidate.py appends it to the draft's text.
"""
import os
import sys

import docker_image
import names
from common import DIST, ROOT, run, version

TARBALLS = "target/dist/kiku-engine-*-linux-x64.tar.gz"


def main():
    """Build, copy the tarball into dist/ and write NOTES.md; return the exit code."""
    DIST.mkdir(exist_ok=True)
    code = docker_image.run_script(docker_image.script(
        "cargo test --release --locked -p kiku-core --tests",
        "./scripts/pack-linux.sh",
        f"mkdir -p dist && cp {TARBALLS} dist/",
        owned=("third_party", "target", "dist"),
    ))
    if code:
        return code
    tarball = DIST / names.tarball(version())
    if not tarball.is_file():
        sys.exit(f"build: {tarball} was not made")
    repo = os.environ.get("GITHUB_REPOSITORY")
    if repo:
        (DIST / "NOTES.md").write_text(names.downloads(repo, version()), encoding="utf-8", newline="\n")
    else:
        print("build: GITHUB_REPOSITORY is not set, so no NOTES.md")
    print(f"build: {tarball.relative_to(ROOT).as_posix()} ({tarball.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
