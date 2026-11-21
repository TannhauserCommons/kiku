"""tarball: download the published release's Linux tarball into image-context/, the Docker image's build folder."""
import os
import sys

import names
from common import ROOT, run, version

CONTEXT = ROOT / "image-context"


def main():
    """Download v<version>'s tarball (the checked-out tag's version); return the exit code."""
    release, repo = version(), os.environ.get("GITHUB_REPOSITORY")
    run(["gh", "release", "download", f"v{release}", *(["--repo", repo] if repo else []),
         "--pattern", names.tarball(release), "--dir", str(CONTEXT), "--clobber"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
