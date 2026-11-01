# /// script
# requires-python = ">=3.10"
# dependencies = ["cryptography==46.0.3"]
# ///
"""Signs Kiku's update manifest. Run with `uv run scripts/release.py <command>`.

  keygen
      Prints a new ed25519 private key (hex) for KIKU_SIGNING_KEY. scripts/setup-release.ps1 pipes it
      straight into a CI secret; it is never written to disk.
  deploy-key <folder>
      Writes a new SSH key pair, <folder>/deploy and deploy.pub (OpenSSH, ed25519, no passphrase), for the
      deploy key that publishes the source.
  public-key
      Prints the public key of KIKU_SIGNING_KEY, which builds compile in as KIKU_UPDATE_KEY.
  manifest <installer> <version> <releases> <notes> <out>
      Writes latest.json for the installer published as <releases>/download/v<version>/<installer's name>:
      its size, sha256 and notes, and <releases> as moved_to, so copies built for an older address switch
      to it; signed with KIKU_SIGNING_KEY.

The signed text must match `Release::signed_text` in crates/core/src/update.rs.
"""
import hashlib, json, os, pathlib, sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, NoEncryption, PrivateFormat, PublicFormat

SIGNED_PREFIX = 'kiku-update-v1'


def signing_key():
    seed = os.environ.get('KIKU_SIGNING_KEY', '').strip()
    if len(seed) != 64:
        sys.exit('KIKU_SIGNING_KEY must hold the 64 hex digits keygen printed')
    return Ed25519PrivateKey.from_private_bytes(bytes.fromhex(seed))


def signed_text(release):
    fields = [SIGNED_PREFIX, release['version'], release['url'], str(release['size']), release['sha256'], release['moved_to'], release['notes']]
    return '\n'.join(fields).encode('utf-8')


def manifest(installer, version, releases, notes):
    installer = pathlib.Path(installer)
    data = installer.read_bytes()
    url = f'{releases}/download/v{version}/{installer.name}'
    release = {'version': version, 'url': url, 'size': len(data), 'sha256': hashlib.sha256(data).hexdigest(), 'moved_to': releases, 'notes': notes}
    release['signature'] = signing_key().sign(signed_text(release)).hex()
    return release


def main(command, *args):
    if command == 'keygen':
        print(os.urandom(32).hex())
    elif command == 'deploy-key' and len(args) == 1:
        key, folder = Ed25519PrivateKey.generate(), pathlib.Path(args[0])
        (folder / 'deploy').write_bytes(key.private_bytes(Encoding.PEM, PrivateFormat.OpenSSH, NoEncryption()))
        (folder / 'deploy.pub').write_bytes(key.public_key().public_bytes(Encoding.OpenSSH, PublicFormat.OpenSSH) + b' kiku-release\n')
    elif command == 'public-key':
        print(signing_key().public_key().public_bytes(Encoding.Raw, PublicFormat.Raw).hex())
    elif command == 'manifest' and len(args) == 5:
        installer, version, releases, notes, out = args
        text = json.dumps(manifest(installer, version, releases, notes), indent=2, ensure_ascii=False) + '\n'
        pathlib.Path(out).write_text(text, encoding='utf-8')
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main(*sys.argv[1:] or ['help'])
