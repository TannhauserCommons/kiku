# /// script
# requires-python = ">=3.10"
# dependencies = ["cryptography==46.0.3"]
# ///
"""verify_manifest <public key hex> <manifest.json>...: fail unless each manifest is signed by that key.

Run with `uv run`, like scripts/release.py, whose signed_text it uses, so the signed text has one source.
"""
import json
import pathlib
import sys

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from release import signed_text  # noqa: E402


def main(key, paths):
    """Check each manifest; return the exit code."""
    public = Ed25519PublicKey.from_public_bytes(bytes.fromhex(key))
    failed = False
    for path in map(pathlib.Path, paths):
        try:
            release = json.loads(path.read_text(encoding="utf-8"))
            public.verify(bytes.fromhex(release["signature"]), signed_text(release))
            print(f"verify: {path.name} is signed by {key}")
        except (OSError, ValueError, KeyError, InvalidSignature) as error:
            print(f"verify: {path.name} is not validly signed by {key} ({type(error).__name__})")
            failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    sys.exit(main(sys.argv[1], sys.argv[2:]))
