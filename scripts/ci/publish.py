"""publish: sign the update manifests and make the stable-name copies, in dist/ of the candidate's own tree.

Runs in the release job on the candidate's files. Safe to run twice: manifests are deterministic (ed25519),
copies are of the same bytes, and only the versioned installers and tarball are read, never the copies.
Needs KIKU_SIGNING_KEY and KIKU_RELEASES; RELEASE_NOTES is the note the update offer shows. Nothing leaves
the folder, so DRY_RUN changes nothing here.

Builds compile in crates/core/update-key.txt, so before signing the key must be the one that file holds, and
each manifest is verified against it afterwards: a shipped copy refuses an update signed by any other key,
and cannot be recalled.
"""
import os
import shutil
import subprocess
import sys

import names
import privacy
from common import DIST, ROOT, run, version

RELEASE = ROOT / "scripts" / "release.py"
VERIFY = ROOT / "scripts" / "ci" / "verify_manifest.py"
UPDATE_KEY = ROOT / "crates" / "core" / "update-key.txt"


def needed(release):
    """The files a release must hold, by name."""
    return [names.installer(release, arch) for arch in names.ARCHS] + [names.tarball(release)]


def signing_public_key():
    """The public half of KIKU_SIGNING_KEY, or None (release.py says why on stderr; it never prints the secret)."""
    done = subprocess.run(["uv", "run", str(RELEASE), "public-key"], capture_output=True, text=True)
    if done.returncode:
        print(done.stderr.strip().splitlines()[-1] if done.stderr.strip() else "publish: could not read the signing key")
        return None
    return done.stdout.strip()


def key_matches():
    """Whether the signing key's public half is the committed one."""
    committed, derived = UPDATE_KEY.read_text(encoding="ascii").strip(), signing_public_key()
    if derived is None:
        return False
    if derived != committed:
        print("publish: KIKU_SIGNING_KEY does not match crates/core/update-key.txt; shipped copies would refuse this release's updates")
        print(f"publish: the signing key's public half is {derived}, the file holds {committed}")
        return False
    return True


def main():
    """Check the key, sign, copy, verify, gate; return the exit code."""
    release = version()
    absent = [name for name in needed(release) if not (DIST / name).is_file()]
    if absent:
        print(f"publish: {DIST} lacks {', '.join(absent)}")
        return 1
    releases, notes = os.environ.get("KIKU_RELEASES", ""), os.environ.get("RELEASE_NOTES", "")
    if not releases:
        print("publish: KIKU_RELEASES is not set")
        return 1
    if not key_matches():
        return 1
    for arch, (stable, _) in names.ARCHS.items():
        setup = DIST / names.installer(release, arch)
        run(["uv", "run", str(RELEASE), "manifest", str(setup), release, releases, notes, str(DIST)])
        shutil.copyfile(setup, DIST / stable)
    shutil.copyfile(DIST / names.tarball(release), DIST / names.STABLE_TARBALL)
    manifests = sorted(DIST.glob("latest*.json"))
    run(["uv", "run", str(VERIFY), UPDATE_KEY.read_text(encoding="ascii").strip(), *map(str, manifests)])
    # Installers and the tarball were gated unpacked by smoke; their own bytes can't hold readable text.
    if privacy.gate(*manifests):
        return 1
    print(f"publish: signed and verified {', '.join(m.name for m in manifests)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
