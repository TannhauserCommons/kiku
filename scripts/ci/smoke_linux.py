"""smoke (Linux): the tarball in dist/ passes the privacy gate unpacked and runs on clean Ubuntu and Debian."""
import sys
import tarfile
import tempfile

import names
import privacy
from common import DIST, first_model_id, lists_model, run_limited, version

SYSTEMS = ("ubuntu:22.04", "debian:12")


def gate(tarball):
    """Whether the privacy gate passes on the unpacked tarball (an archive's own bytes can't hold readable text)."""
    with tempfile.TemporaryDirectory(prefix="kiku-smoke-", ignore_cleanup_errors=True) as folder:
        with tarfile.open(tarball) as archive:
            archive.extractall(folder, filter="data")
        return privacy.gate(folder) == 0


def lists_models_on(system, tarball):
    """Unpack the tarball in a clean container of `system` and run `kiku-engine --models`; True when it lists the catalog."""
    script = f"cd /tmp && tar xzf /dist/{tarball.name} && ./kiku-engine/kiku-engine --models"
    code, output = run_limited(["docker", "run", "--rm", "-v", f"{DIST}:/dist:ro", system, "sh", "-c", script], 300)
    ok = code == 0 and lists_model(output, first_model_id())
    print(f"smoke: {system}: {'ok' if ok else 'FAILED'}")
    if not ok:
        print("\n".join(output.splitlines()[-5:]))
    return ok


def main():
    """Check the tarball and return the exit code."""
    tarball = DIST / names.tarball(version())
    if not tarball.is_file():
        print(f"smoke: {tarball} is missing")
        return 1
    if not gate(tarball):
        return 1
    results = [lists_models_on(system, tarball) for system in SYSTEMS]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
