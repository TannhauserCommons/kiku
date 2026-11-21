"""engine: the Linux engine's tests, in the Linux build image (what release.yml's linux job ran).

Builds scripts/linux-build.Dockerfile, then in a container fetches the runtime and builds Moonshine when
they are missing and runs the kiku-core tests (docker_image.py says where the files live).
"""
import sys

import docker_image


def main():
    """Run the tests in the image and return the exit code."""
    return docker_image.run_script(docker_image.script("cargo test --release --locked -p kiku-core --tests"))


if __name__ == "__main__":
    sys.exit(main())
