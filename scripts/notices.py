"""Writes THIRD-PARTY-NOTICES.txt for a release: installer/notices.txt (the libraries, runtimes and
models), then the licence files of the libraries Moonshine builds in and of every Rust crate built into
Kiku's programs, each text once.

  python scripts/notices.py <out> [--engine]

--engine writes the notices of the Linux package of kiku-engine instead: its own header and the Rust
crates kiku-core links on Linux.
"""
import json, pathlib, re, subprocess, sys

ENGINE_HEADER = """THIRD-PARTY NOTICES FOR KIKU'S MODEL HOST (kiku-engine, Linux)

kiku-engine is built with, ships with, and downloads when you ask, the software and models below, made by
others under their own licences. Their notices and licence terms follow.


INCLUDED WITH THE PACKAGE
=========================

ONNX Runtime 1.28 (libonnxruntime.so, libonnxruntime_providers_cuda.so, libonnxruntime_providers_shared.so)
  Copyright (c) Microsoft Corporation. MIT License (text below).

sherpa-onnx 2.0 (libsherpa-onnx-c-api.so, libsherpa-onnx-cxx-api.so)
  By the k2-fsa project (https://github.com/k2-fsa/sherpa-onnx). Apache License 2.0; its full text is
  reproduced with the Rust crates below that use it.

Moonshine 0.1 runtime (built into kiku-engine)
  Copyright (c) 2025 Useful Sensors, Inc. (dba Moonshine AI). MIT License (text below). The libraries
  it builds in are listed further down with their licences.


NOT INCLUDED, USED WHEN PRESENT
===============================

NVIDIA CUDA 12 and cuDNN 9, for running on a GPU, come from your system or, in Kiku's Docker image, from
NVIDIA's CUDA image, under NVIDIA's licence terms for them
(https://docs.nvidia.com/cuda/eula/index.html, https://docs.nvidia.com/deeplearning/cudnn/latest/reference/eula.html).


DOWNLOADED WHEN YOU ASK FOR THEM (kiku-engine --download)
=========================================================

Silero VAD (silero_vad.onnx)
  Copyright (c) 2020-present Silero Team. MIT License (text below).

Parakeet TDT 0.6B v2 and v3 speech models
  By NVIDIA, under the Creative Commons Attribution 4.0 International licence (CC BY 4.0,
  https://creativecommons.org/licenses/by/4.0/). ONNX exports by the k2-fsa sherpa-onnx project,
  downloaded from Hugging Face.

Parakeet Unified EN 0.6B speech model
  By NVIDIA, under the NVIDIA Open Model License
  (https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/). ONNX
  exports by the k2-fsa sherpa-onnx project, downloaded from Hugging Face. The fp16 version, published
  with Kiku's releases, is that export with its weights stored as 16-bit numbers.

Moonshine streaming speech models
  Copyright (c) 2025 Useful Sensors, Inc. (dba Moonshine AI). MIT License (text below). Downloaded
  from Hugging Face.


"""

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


def rust_crates(engine):
    """Crates the programs link (normal dependencies, not build or dev ones): the workspace's on Windows,
    or with `engine` kiku-core's on Linux."""
    platform = 'x86_64-unknown-linux-gnu' if engine else 'x86_64-pc-windows-msvc'
    metadata = json.loads(subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--locked', '--filter-platform', platform],
        cwd=ROOT, capture_output=True, text=True, check=True).stdout)
    packages = {p['id']: p for p in metadata['packages']}
    nodes = {n['id']: n for n in metadata['resolve']['nodes']}
    members = set(metadata['workspace_members'])
    roots = [id for id in members if packages[id]['name'] == 'kiku-core'] if engine else list(members)
    seen, todo = set(), list(roots)
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


def engine_header():
    """The engine's header and the MIT text of installer/notices.txt, which its entries point to."""
    shared = (ROOT / 'installer' / 'notices.txt').read_text(encoding='utf-8')
    return ENGINE_HEADER + shared[shared.index('MIT LICENSE\n'):shared.index('ISC LICENSE\n')].rstrip() + '\n'


def main(out, engine):
    notices = engine_header() if engine else (ROOT / 'installer' / 'notices.txt').read_text(encoding='utf-8')
    notices += section('LIBRARIES BUILT INTO MOONSHINE', moonshine_libraries())
    notices += section('RUST CRATES BUILT INTO KIKU', rust_crates(engine))
    pathlib.Path(out).write_text(notices, encoding='utf-8', newline='\n' if engine else '\r\n')


if __name__ == '__main__':
    if len(sys.argv) not in (2, 3) or sys.argv[2:] not in ([], ['--engine']):
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2:] == ['--engine'])
