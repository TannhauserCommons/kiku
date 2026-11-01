"""Keeps crates/core/catalog.json in step with the files it names.

  python scripts/catalog.py hash <folder> [<folder> ...]
      Fills in the size and sha256 of every file from local copies; changed ones are listed.
  python scripts/catalog.py stage <out> <folder> [<folder> ...]
      Copies the files published as our own release assets (URLs under "release") to <out>, named
      as the catalog's URLs name them and checked against their sha256, ready to upload.

Each folder is searched as a models root (`<folder>/<dir>/<path>`) and as a flat folder of files
(`<folder>/<path>`, e.g. the GPU pack's DLLs in third_party/cuda-runtime).
"""
import hashlib, json, pathlib, shutil, sys

CATALOG = pathlib.Path(__file__).resolve().parent.parent / 'crates' / 'core' / 'catalog.json'
RELEASE = '{release}/'


def sha256(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as file:
        for block in iter(lambda: file.read(1 << 22), b''):
            digest.update(block)
    return digest.hexdigest()


def find(folders, dir, name):
    for folder in folders:
        for candidate in (folder / dir / name, folder / name):
            if candidate.is_file():
                return candidate


def entries(catalog):
    """Every (folder, file entry) in the catalog."""
    groups = [(v['dir'], v['files']) for m in catalog['models'] for v in m['variants']]
    groups += [(c['dir'], c['files']) for c in catalog['components']]
    return [(dir, entry) for dir, files in groups for entry in files]


def hash_files(folders):
    catalog = json.loads(CATALOG.read_text(encoding='utf-8'))
    for dir, entry in entries(catalog):
        local = find(folders, dir, entry['path'])
        if local is None:
            continue
        size, digest = local.stat().st_size, sha256(local)
        if (size, digest) != (entry['size'], entry['sha256']):
            print(f'{dir}/{entry["path"]}: {entry["size"]} {entry["sha256"][:12]} -> {size} {digest[:12]}')
        entry['size'], entry['sha256'] = size, digest
    CATALOG.write_text(json.dumps(catalog, indent=2, ensure_ascii=False) + '\n', encoding='utf-8')


def stage(out, folders):
    out.mkdir(parents=True, exist_ok=True)
    catalog = json.loads(CATALOG.read_text(encoding='utf-8'))
    missing = []
    for dir, entry in entries(catalog):
        if not entry['url'].startswith(RELEASE):
            continue
        local = find(folders, dir, entry['path'])
        if local is None or sha256(local) != entry['sha256']:
            missing.append(f'{dir}/{entry["path"]}')
            continue
        shutil.copyfile(local, out / entry['url'][len(RELEASE):])
    print(f'staged in {out}; base URL {catalog["release"]} ({{releases}} is the KIKU_RELEASES builds are given)')
    if missing:
        sys.exit('missing or different: ' + ', '.join(missing))


if __name__ == '__main__':
    command, args = sys.argv[1], [pathlib.Path(a) for a in sys.argv[2:]]
    if command == 'hash':
        hash_files(args)
    elif command == 'stage':
        stage(args[0], args[1:])
    else:
        sys.exit(__doc__)
