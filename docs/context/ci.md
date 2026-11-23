# CI

Every CI step is one `mise run <task>`; the tasks are in `mise.toml` (shared `ci:*` tasks in `.github/ci/tasks.toml`, copied from the CI skeleton and never edited here). Tools and the Rust version are pinned: `mise.toml` and `mise.lock` for python, uv, actionlint and shellcheck (which actionlint runs on every `run:` script); `rust-toolchain.toml` for Rust, which the Linux build image installs on the first `cargo` call in `/src`.

## One public repository

Kiku is developed in public, in `TannhauserCommons/kiku`: what is committed is published once it is pushed, before any CI runs. `main` is the trunk and carries the next release; a pull request changes it, and a release tags the green commit (`CI_BRANCH` is `main` in every workflow). Public runners are free, so every job runs on every change that needs it.

The files that must stay private (agent instructions and skills, build loops, spikes, release set-up scripts, `.private-words`) are gitignored, in the `BEGIN private` block of `.gitignore`: they stay on disk and are never committed. A name alone there (`AGENTS.md`, `skills/`) matches at any depth; a path with a slash (`/docs/RELEASING.md`) is from the root. Nothing public may name a private word, so the checks below run before a commit, before a push and in CI.

## Git hooks

`.githooks/pre-commit` and `.githooks/pre-push` are in the repository; `mise run setup` (once per checkout, safe to run again) runs `python scripts/ci/hooks.py install`, which sets `core.hooksPath` to `.githooks`. Both are short `sh` scripts that run `scripts/ci/hooks.py` with `KIKU_PYTHON`, `python3` or `python` (and refuse when none works). The gate logic is the CI's own: `scripts/ci/privacy.py` and `scripts/privacy-gate.py`.

- `pre-commit` refuses the commit when the index lists a private path (a `git add -f` included; fix it with `git rm --cached`), or when `privacy-gate.py` finds a pattern or a private word in the index's files (the staged content, not the working copy).
- `pre-push` reads the ref updates git gives it and, for every commit the remote lacks (a new branch is measured against the remote's known refs), refuses a private path anywhere in its tree, runs the gate over its tree and looks for a private word in its author, committer and message and for a tool credited as an author. A commit that was clean when it was made but not when pushed (amended, rebased, or made with the hooks off) is caught here.
- The words are `KIKU_PRIVATE_WORDS` (comma-separated) plus `.private-words` (base64 of comma-separated words, in the checkout or the main checkout of a worktree). They are never printed. With no word at all both hooks refuse with a message, because a public push the gate could not fully check is the leak to prevent.
- Outside contributors, who have no words, set `KIKU_PATTERNS_ONLY=1` to run the hooks on the patterns and the private paths alone. CI ignores it and still requires the words.
- `python -m unittest discover -s scripts/ci` runs `test_hooks.py`, which makes temporary repositories and checks that git itself refuses a commit and a push carrying a made-up word or a force-added private file.

## Tasks

| Task | Does | Runs |
| --- | --- | --- |
| `version` | Prints the version in `[workspace.package]` of `Cargo.toml` | Anywhere |
| `bump <x.y.z>` | Writes it there and in the `Cargo.lock` entries of the workspace's own packages; no cargo needed | Anywhere |
| `check` | The privacy gate over the public tree, workflow lint, `ci:switches` | `check.yml`'s `check` job, on every pull request to `main` and every push to it |
| `engine` | `kiku-core`'s tests on Linux, in the image built from `scripts/linux-build.Dockerfile` (runtime fetched and Moonshine built when missing) | `check.yml`'s `engine` job on Ubuntu 22.04, when its paths changed or on `main`; on Windows through Docker Desktop |
| `windows` | `scripts/ci/windows.py`: fetches the runtime and builds Moonshine when `third_party` lacks them, runs `cargo test --release --locked --workspace --tests` and, when `KIKU_SNAPSHOTS` names a folder, the settings snapshots (`cargo test -p kiku snapshots -- --ignored`). Windows only | `check.yml`'s `windows` job on windows-2025, when its paths changed or on `main`; the snapshots are uploaded as the `snapshots` artifact. By hand on this PC before a push |
| `build` | Tests and builds this runner's release target into `dist/` (made if missing, other files left): on Windows `scripts/ci/build_windows.py` fetches the runtime and builds Moonshine if `third_party` lacks them, runs `scripts/build-release.ps1 -Test`, then Inno Setup (`ISCC.exe`, installed pinned and hash-checked if missing) for the PC's own architecture and copies `Kiku-<version>-<arch>-setup.exe`; with `KIKU_RELEASES` set it compiles in that and `crates/core/update-key.txt`. On Linux `scripts/ci/build_linux.py` tests and runs `scripts/pack-linux.sh` in the build image and copies `kiku-engine-<version>-linux-x64.tar.gz`; with `GITHUB_REPOSITORY` set it also writes `dist/NOTES.md`, the release text's "Downloads" list (`scripts/ci/names.py`) | `main.yml`'s `app` (windows-2025 x64, windows-11-arm ARM64) and `linux` (ubuntu-22.04) jobs, on every push to `main`; on this PC through Docker Desktop for Linux |
| `smoke` | Tests what `build` made. Windows: privacy gate on `target\dist\Kiku`; then, only when `GITHUB_ACTIONS` is `true`, a silent install of the installer into a temp folder (`/VERYSILENT`, 10 minute limit; the silent run starts Kiku, which is stopped again), every staged file compared with the installed one, and `kiku-engine.exe --models` from there; outside CI the engine check runs on the staged folder, since an install replaces a real one's uninstall entry and shortcut. Linux: privacy gate on the unpacked tarball, then `kiku-engine --models` in clean `ubuntu:22.04` and `debian:12` containers. Both expect the catalog's first model id | `main.yml`'s `app` and `linux` jobs, after `build` |
| `publish` | Run in the candidate's tree on `dist/`: `scripts/release.py manifest` for each installer (`latest.json` x64, `latest-arm64.json`; needs `KIKU_SIGNING_KEY`, `KIKU_RELEASES`; `RELEASE_NOTES` is the note), the stable-name copies (`Kiku-setup.exe`, `Kiku-arm64-setup.exe`, `kiku-engine-linux-x64.tar.gz`) and the privacy gate on the manifests. The installers and the tarball are not gated here: `smoke` gated what they hold unpacked. Fails when an installer or the tarball is missing. A second run gives the same files | `release.yml`'s `release` job |
| `tarball` | Downloads the published release's tarball for the checked-out tag into `image-context/` | `release.yml`'s `image` job |
| `image` | Builds `Dockerfile` from `image-context/` and pushes `ghcr.io/<owner>/kiku-engine:<version>` and `:latest` (login with `GH_TOKEN` on stdin); `DRY_RUN=1` builds only | `release.yml`'s `image` job |
| `models` | `scripts/publish-models.ps1`: the model files the catalog serves from `models-v1`, made and uploaded when missing | `release.yml`'s `models` job (windows-2025). Windows only; it publishes to GitHub |
| `setup` | Points git at `.githooks/` (see Git hooks) | By hand, once per checkout |

`check.yml` (jobs `check`, `engine`, `windows`, then `gate`) runs on every pull request to `main` and (called by `main.yml` with `all: true`) on every push to `main`. A change reaches `main` through a pull request, so a pushed branch is checked once it has one. Its `gate` job is the one status to require (a ruleset on `main`).

## What `check` does

- Privacy gate: fails when a tracked file is a private path (`.gitignore`'s `BEGIN private` block), then copies the tracked files to a temp folder and runs `scripts/privacy-gate.py` on it with the private words: `KIKU_PRIVATE_WORDS`, plus `.private-words` on a machine that has it. Only tracked files are read, so a new file is gated once it is added.
- In CI every privacy gate (`privacy.gate`: `check`, both `smoke` tasks and `publish`) fails with "no private words" when there are none, instead of passing on patterns alone: on pushes, on pull requests from this repository and on dispatches. A pull request from a fork gets no secrets; the workflow sets `KIKU_FORK_PR=true` for it and its gate checks the patterns only. Local runs without words gate the patterns too (the hooks are stricter: see Git hooks). The words are the repository secret `KIKU_PRIVATE_WORDS`, read by `check`, `app`, `linux` and `release`.
- Unit tests: `python -m unittest discover -s scripts/ci` (the `test_*.py` beside the scripts).
- Lint: `mise run ci:lint` (`actionlint` and `.github/ci/lint.py`) on every workflow.

## The release path

1. A push to `main` (a merged pull request) runs `main.yml`: `check`, then `app` (x64 and ARM64, each built natively), `linux`, and, when all are green, `candidate` (`mise run ci:candidate`), which puts the files in `dist/` on the standing draft release. `candidate.py` appends `NOTES.md` (written by the `linux` job) to the draft's text.
2. The owner dispatches `release.yml` on `main` (inputs `dry_run`, `notes`); the `release` environment asks for the owner's approval. `release` fetches the candidate (`ci:fetch`), runs `publish` in its tree and publishes the draft (`ci:publish`, which creates the tag on the candidate's commit). Then `image` and `models` run unless `dry_run`. Every step can be run again: `ci:fetch` resumes a release that published and then failed, `publish` does nothing new the second time, `image` pushes the same tags, `publish-models.ps1` skips files that are there.
3. The version is bumped by hand, in a pull request (`mise run bump <x.y.z>`): the release neither bumps nor moves any branch, the tag is created on the candidate's commit. `candidate` records nothing while the version in `Cargo.toml` already has a tag ("already released; this commit is not a candidate"), so after a release the next candidate comes from the first green `main` run that carries a new version. Every green run replaces the draft, so the candidate is the newest such commit; `ci:fetch` refuses a draft that is not on `main` or fails its hash checks, and says "no candidate" when there is none. Re-running an old release run would move the Docker `latest` tag back, so a release run is re-run only to finish it.

`crates/core/update-key.txt` is the public half of the update signing key (one line of 64 hex digits, public: every shipped Kiku holds it). `build` on Windows compiles it in as `KIKU_UPDATE_KEY` when `KIKU_RELEASES` is set. The matching private key is the `KIKU_SIGNING_KEY` secret of the `release` environment (required reviewer: the owner; branch policy: `main`). `KIKU_RELEASES` is the repository variable of that name, else `https://github.com/<repository>/releases`.

Before signing, `publish` derives the public key from `KIKU_SIGNING_KEY` (`release.py public-key`) and fails unless it equals `crates/core/update-key.txt`: copies built from that file refuse updates signed by any other key, and shipped copies cannot be recalled. After signing it verifies each manifest against the file's key (`scripts/ci/verify_manifest.py`, run with `uv`, which takes the signed text from `release.py`'s `signed_text`).

Windows runners: when `ISCC.exe` is missing, `build` downloads Inno Setup 6.7.1 from its GitHub release (`jrsoftware/issrc`, tag `is-6_7_1`; `files.jrsoftware.org` holds only signatures), checks the file against the SHA-256 pinned in `scripts/ci/build_windows.py` (the digest GitHub records for it, not checked against a local download), and runs it silently with a 10 minute limit. Chocolatey is not used. `ISCC.exe` is an x86 program, run emulated on the ARM64 runner.

## Caches

`engine` restores the runtime (`third_party`) and cargo's registry and `target` (`target/cargo-registry`) with `actions/cache/restore` and saves them with `actions/cache/save` only on `main`. `main.yml`'s `linux` job restores the same runtime and a cargo `target` of its own key (`cargo-dist-linux-x64-...`), since its build also makes the packing build; its `app` jobs restore the Windows runtime (`runtime-<arch>-...`: `third_party/sherpa-onnx`, `moonshine-src`, `moonshine-build`). Only `main` saves them.

Cargo's caches are keyed on `Cargo.lock`, so each version bump saves new ones and GitHub evicts the oldest. Only `main` saves caches, so pull requests, which read the default branch's caches, start from `main`'s.
