"""Fails when anything a release ships, or its published source, could tell who built it.

  python scripts/privacy-gate.py <file or folder> [...]

Every file is searched as bytes and as UTF-16 text (Windows resources) for the patterns below, and for
private words: KIKU_PRIVATE_WORDS (comma-separated; a CI secret) and this machine's user and computer
names. Words are matched ignoring case and never printed.
"""
import os, pathlib, re, sys

PATTERNS = {
    'a Windows user folder': r'[A-Za-z]:[\\/]+Users[\\/]+',
    'a Unix home folder': r'/(?:home|Users)/[\w.-]+/',
    'a Rust or Cargo home': r'[\\/]\.(?:cargo|rustup)[\\/]+',
    'an email address': r'(?<![\w.%+-])[\w.%+-]{2,}@[A-Za-z0-9-]{2,}(?:\.[A-Za-z0-9-]+)*\.[a-z]{2,6}(?![A-Za-z])',
    "this CI's workspace": r'[A-Za-z]:[\\/]+a[\\/]+kiku',
    'a tool credited as an author': r'(?i)co-authored-by[:]|generated with \[?claude|noreply@anthropic\.com',
}
# Addresses third-party code ships with: a CA certificate's (webpki-roots) and OpenSSL's assembly credits (ring).
ALLOWED = {'info@e-szigno.hu', 'appro@openssl.org'}
# Domains reserved for examples (RFC 2606), which no one's address can be at; tests and docs use them.
EXAMPLE_DOMAINS = ('@example.com', '@example.org', '@example.net')
# Files quoting other people's licences, copyright lines and addresses included; private words still count.
QUOTES = {'THIRD-PARTY-NOTICES.txt': {'an email address'}}
# Upstream libraries shipped as built (builders' paths, random bytes that look like addresses); only the
# private words in KIKU_PRIVATE_WORDS are looked for in them.
UPSTREAM = {'libonnxruntime.so', 'libonnxruntime_providers_cuda.so', 'libonnxruntime_providers_shared.so',
            'libsherpa-onnx-c-api.so', 'libsherpa-onnx-cxx-api.so'}
# Below this length a word matches too much by chance to say anything.
SHORTEST_WORD = 4


def explicit_words():
    return sorted({w.strip() for w in os.environ.get('KIKU_PRIVATE_WORDS', '').split(',') if len(w.strip()) >= SHORTEST_WORD},
                  key=len, reverse=True)


def private_words():
    words = explicit_words()
    # A CI runner's names are its own (e.g. "runner"), not the owner's; there the explicit words cover them.
    if os.environ.get('GITHUB_ACTIONS') != 'true':
        words += [os.environ.get(name, '') for name in ('USERNAME', 'USER', 'COMPUTERNAME')]
    return sorted({w for w in words if len(w) >= SHORTEST_WORD}, key=len, reverse=True)


def texts(path):
    """The file as Latin-1 (byte for byte) and as UTF-16 at both byte alignments."""
    data = path.read_bytes()
    return [data.decode('latin-1'), data.decode('utf-16-le', 'ignore'), data[1:].decode('utf-16-le', 'ignore')]


def findings(path, words):
    found = set()
    upstream = path.name in UPSTREAM
    if upstream:
        words = explicit_words()
    for text in texts(path):
        for what, pattern in PATTERNS.items():
            if upstream or what in QUOTES.get(path.name, ()):
                continue
            for match in re.finditer(pattern, text, re.ASCII):
                if match.group() not in ALLOWED and not match.group().lower().endswith(EXAMPLE_DOMAINS):
                    found.add(f'{what}: {redact(match.group(), words)}')
        lowered = text.lower()
        found.update('a private word' for word in words if word.lower() in lowered)
    return found


def redact(text, words):
    for word in words:
        text = re.sub(re.escape(word), '***', text, flags=re.IGNORECASE)
    return text


def files(paths):
    """The files in `paths`, folders searched through, except git's own (a source snapshot's `.git`)."""
    for path in map(pathlib.Path, paths):
        found = (p for p in sorted(path.rglob('*')) if p.is_file() and '.git' not in p.relative_to(path).parts)
        yield from found if path.is_dir() else [path]


def main(paths):
    words, failed = private_words(), False
    for path in files(paths):
        for finding in sorted(findings(path, words)):
            print(f'{path.name}: {finding}')
            failed = True
    if failed:
        sys.exit('privacy gate: the files above must not ship')
    print(f'privacy gate: nothing found ({len(words)} private words, {len(PATTERNS)} patterns)')


if __name__ == '__main__':
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
