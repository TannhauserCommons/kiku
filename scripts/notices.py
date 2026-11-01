"""Writes THIRD-PARTY-NOTICES.txt for a release: installer/notices.txt (the libraries, runtimes and
models), then the licence files of the libraries Moonshine builds in and of every Rust crate built into
Kiku's programs, each text once.

  python scripts/notices.py <out>
"""
import json, pathlib, re, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
MOONSHINE_LIBRARIES = ROOT / 'third_party' / 'moonshine-src' / 'core' / 'third-party'
# doctest is only in Moonshine's tests; ONNX Runtime is listed in the header.
NOT_BUILT_IN = {'doctest', 'onnxruntime'}
LICENCE_FILE = re.compile(r'(licen[cs]e|copying|notice|unlicense)', re.IGNORECASE)


def licence_files(folder):
    return sorted(p for p in folder.iterdir() if p.is_file() and LICENCE_FILE.match(p.name))


def moonshine_libraries():
    for folder in sorted(MOONSHINE_LIBRARIES.iterdir()):
        if folder.is_dir() and folder.name not in NOT_BUILT_IN:
            files = licence_files(folder) or [p for d in folder.iterdir() if d.is_dir() for p in licence_files(d)]
            yield folder.name, None, files


def rust_crates():
    """Crates the workspace's programs link (normal dependencies, not build or dev ones), on Windows."""
    metadata = json.loads(subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--locked', '--filter-platform', 'x86_64-pc-windows-msvc'],
        cwd=ROOT, capture_output=True, text=True, check=True).stdout)
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    members, seen, todo = set(metadata['workspace_members']), set(), list(metadata['workspace_members'])
    while todo:
        id = todo.pop()
        if id not in seen:
            seen.add(id)
            todo += [d['pkg'] for d in nodes[id]['deps'] if any(k['kind'] is None for k in d['dep_kinds'])]
    for package in sorted((packages[id] for id in seen - members), key=lambda p: (p['name'], p['version'])):
        folder = pathlib.Path(package['manifest_path']).parent
        yield f'{package["name"]} {package["version"]}', package['license'], licence_files(folder)


def section(title, entries):
    lines, texts = [f'\n\n{title}\n{"=" * len(title)}\n'], {}
    for name, licence, files in entries:
        lines.append(f'\n--- {name}' + (f' ({licence})' if licence else '') + '\n')
        for file in files:
            text = file.read_text(encoding='utf-8', errors='replace').strip()
            if text in texts:
                lines.append(f'{file.name}: the same text as {texts[text]}\n')
            else:
                texts[text] = f'{name}, {file.name}'
                lines.append(f'{file.name}:\n\n{text}\n\n')
    return ''.join(lines)


def main(out):
    notices = (ROOT / 'installer' / 'notices.txt').read_text(encoding='utf-8')
    notices += section('LIBRARIES BUILT INTO MOONSHINE', moonshine_libraries())
    notices += section('RUST CRATES BUILT INTO KIKU', rust_crates())
    pathlib.Path(out).write_text(notices, encoding='utf-8', newline='\r\n')


if __name__ == '__main__':
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
