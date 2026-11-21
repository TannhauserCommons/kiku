"""image: build the Docker image from image-context/'s tarball and push it as <version> and latest to ghcr.io.

Needs GITHUB_REPOSITORY, GITHUB_ACTOR and GH_TOKEN (read from the environment and handed to docker login on
stdin). A rerun pushes the same tags again. DRY_RUN=1 builds and pushes nothing.
"""
import os
import subprocess
import sys

import names
from common import ROOT, run, version

CONTEXT = ROOT / "image-context"


def main():
    """Build and push; return the exit code."""
    repo, release = os.environ["GITHUB_REPOSITORY"], version()
    if not (CONTEXT / names.tarball(release)).is_file():
        print(f"image: {CONTEXT} lacks {names.tarball(release)}")
        return 1
    image = names.image(repo)
    tags = [f"{image}:{release}", f"{image}:latest"]
    run(["docker", "build", "-f", str(ROOT / "Dockerfile"), "-t", tags[0], "-t", tags[1],
         "--label", f"org.opencontainers.image.source=https://github.com/{repo}", str(CONTEXT)])
    if os.environ.get("DRY_RUN") == "1":
        print(f"dry run: would push {', '.join(tags)}")
        return 0
    login = ["docker", "login", "ghcr.io", "-u", os.environ["GITHUB_ACTOR"], "--password-stdin"]
    if subprocess.run(login, input=os.environ["GH_TOKEN"], text=True).returncode:
        return 1
    for tag in tags:
        run(["docker", "push", tag])
    return 0


if __name__ == "__main__":
    sys.exit(main())
