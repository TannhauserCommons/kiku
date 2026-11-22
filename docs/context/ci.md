# CI

Every CI step is one `mise run <task>`; the tasks are in `mise.toml` (shared `ci:*` tasks in `.github/ci/tasks.toml`, copied from the CI skeleton and never edited here). Tools and the Rust version are pinned: `mise.toml` and `mise.lock` for python, uv and actionlint; `rust-toolchain.toml` for Rust, which the Linux build image installs on the first `cargo` call in `/src`.

## Two repositories

The same tree is in a private repository (all development) and, less the files `.publicignore` lists, a public one (releases). Every workflow file exists in both and tells them apart with `github.event.repository.private`. The release branch, which carries the next release, is `main` in the private repository and `next` in the public one (`CI_BRANCH`).

The private repository's Actions minutes are limited and every job is billed at least one minute, so it runs few jobs, mostly on Linux (the one Windows job, `windows`, runs only when its paths changed, and counts double). The public repository's minutes are free.

## Tasks

| Task | Does | Runs |
| --- | --- | --- |
| `version` | Prints the version in `[workspace.package]` of `Cargo.toml` | Anywhere |
| `bump <x.y.z>` | Writes it there and in the `Cargo.lock` entries of the workspace's own packages; no cargo needed | Anywhere |
| `check` | The privacy gate over the public tree, workflow lint, `ci:switches` | `check.yml`'s `check` job, on every pull request |
| `engine` | `kiku-core`'s tests on Linux, in the image built from `scripts/linux-build.Dockerfile` (runtime fetched and Moonshine built when missing) | `check.yml`'s `engine` job on Ubuntu 22.04, when its paths changed or on the release branch; on Windows through Docker Desktop, in the volumes `scripts/linux-gate.ps1` uses |
| `windows` | `scripts/ci/windows.py`: fetches the runtime and builds Moonshine when `third_party` lacks them, runs `cargo test --release --locked --workspace --tests` and, when `KIKU_SNAPSHOTS` names a folder, the settings snapshots (`cargo test -p kiku snapshots -- --ignored`). Windows only | `check.yml`'s `windows` job on windows-2025, when its paths changed or on the release branch (it replaces `voice-ci.yml`); the snapshots are uploaded as the `snapshots` artifact. By hand on this PC before a push |
| `build` | Tests and builds this runner's release target into `dist/` (made if missing, other files left): on Windows `scripts/ci/build_windows.py` fetches the runtime and builds Moonshine if `third_party` lacks them, runs `scripts/build-release.ps1 -Test`, then Inno Setup (`ISCC.exe`, installed pinned and hash-checked if missing) for the PC's own architecture and copies `Kiku-<version>-<arch>-setup.exe`; with `KIKU_RELEASES` set it compiles in that and `crates/core/update-key.txt`. On Linux `scripts/ci/build_linux.py` tests and runs `scripts/pack-linux.sh` in the build image and copies `kiku-engine-<version>-linux-x64.tar.gz`; with `GITHUB_REPOSITORY` set it also writes `dist/NOTES.md`, the release text's "Downloads" list (`scripts/ci/names.py`) | `main.yml`'s `app` (windows-2025 x64, windows-11-arm ARM64) and `linux` (ubuntu-22.04) jobs, in the public repository on `next`; on this PC through Docker Desktop for Linux |
| `smoke` | Tests what `build` made. Windows: privacy gate on `target\dist\Kiku`; then, only when `GITHUB_ACTIONS` is `true`, a silent install of the installer into a temp folder (`/VERYSILENT`, 10 minute limit; the silent run starts Kiku, which is stopped again), every staged file compared with the installed one, and `kiku-engine.exe --models` from there; outside CI the engine check runs on the staged folder, since an install replaces a real one's uninstall entry and shortcut. Linux: privacy gate on the unpacked tarball, then `kiku-engine --models` in clean `ubuntu:22.04` and `debian:12` containers. Both expect the catalog's first model id | `main.yml`'s `app` and `linux` jobs, after `build` |
| `publish` | Run in the candidate's tree on `dist/`: `scripts/release.py manifest` for each installer (`latest.json` x64, `latest-arm64.json`; needs `KIKU_SIGNING_KEY`, `KIKU_RELEASES`; `RELEASE_NOTES` is the note), the stable-name copies (`Kiku-setup.exe`, `Kiku-arm64-setup.exe`, `kiku-engine-linux-x64.tar.gz`) and the privacy gate on the manifests. The installers and the tarball are not gated here: `smoke` gated what they hold unpacked. Fails when an installer or the tarball is missing. A second run gives the same files | `release.yml`'s `release` job |
| `land` | `land <sha>`: moves the public `main` to the released commit with `git push origin <sha>:refs/heads/main`. Does nothing when `main` is at the commit; fails when `main` is not an ancestor of it; `DRY_RUN=1` only prints | `release.yml`'s `release` job, last, also when the release was published by an earlier run |
| `tarball` | Downloads the published release's tarball for the checked-out tag into `image-context/` | `release.yml`'s `image` job |
| `image` | Builds `Dockerfile` from `image-context/` and pushes `ghcr.io/<owner>/kiku-engine:<version>` and `:latest` (login with `GH_TOKEN` on stdin); `DRY_RUN=1` builds only | `release.yml`'s `image` job |
| `models` | `scripts/publish-models.ps1`: the model files the catalog serves from `models-v1`, made and uploaded when missing | `release.yml`'s `models` job (windows-2025). Windows only; it publishes to GitHub |
| `next` | Pushes the filtered tree as one rolling commit to the public repository's `next` | `main.yml`'s `next` job, private repository only |

`check.yml` (jobs `check`, `engine`, `windows`) runs on every pull request and (called by `main.yml` with `all: true`) on every push to the release branch. A change reaches the release branch through a pull request, so a pushed branch is checked once it has one. Its `gate` job is the one status to require.

## What `check` does

- Privacy gate: copies the files that are, or would be, public to a temp folder and runs `scripts/privacy-gate.py` on it. With a `.publicignore` in the tree (the private repository) that is every tracked file it doesn't list, with the words in `.private-words` added to `KIKU_PRIVATE_WORDS`. Without one (the public repository) it is every tracked file, with the `KIKU_PRIVATE_WORDS` secret, which is empty for outside pull requests. Only tracked files are read, so a new file is gated once it is added.
- In CI on any event but `pull_request` (`GITHUB_ACTIONS` is `true` and `GITHUB_EVENT_NAME` is not `pull_request`), every privacy gate (`privacy.gate`: `check`, `next`, both `smoke` tasks and `publish`) fails with "no private words" when there are none, instead of passing on patterns alone. Pull requests, which get no secrets from forks, and local runs still gate the patterns. On the public side the words are the repository-level secret `KIKU_PRIVATE_WORDS` (there is no `.private-words`), read by `check`, `app`, `linux` and `release`.
- Unit tests: `python -m unittest discover -s scripts/ci` (the `test_*.py` beside the scripts).
- Lint: `actionlint` and `.github/ci/lint.py` on `check.yml`, `main.yml`, `after.yml` and `release.yml`. `publish.yml` (inline shell, unpinned actions) does not meet its rules and is not linted by `check`; `mise run ci:lint` lints every workflow and fails on it until a later unit removes it.

## The release path

1. A green push to the private `main` runs `next`, which pushes the rolling commit to the public `next`.
2. In the public repository a push to `next` runs `main.yml`: `check`, then `app` (x64 and ARM64, each built natively), `linux`, and, when all are green, `candidate` (`mise run ci:candidate`), which puts the files in `dist/` on the standing draft release. `candidate.py` appends `NOTES.md` (written by the `linux` job) to the draft's text.
3. The owner dispatches `release.yml` on `next` (inputs `dry_run`, `notes`). `release` fetches the candidate (`ci:fetch`), runs `publish` in its tree, publishes the draft (`ci:publish`, which creates the tag) and runs `land`. Then `image` and `models` run unless `dry_run`. Every step can be run again: `ci:fetch` resumes a release that published and then failed, `publish` and `land` do nothing new the second time, `image` pushes the same tags, `publish-models.ps1` skips files that are there.
4. The version is bumped in the private repository; the release does not bump.

`crates/core/update-key.txt` is the public half of the update signing key (one line of 64 hex digits, public: every shipped Kiku holds it). `build` on Windows compiles it in as `KIKU_UPDATE_KEY` when `KIKU_RELEASES` is set. The matching private key is the `KIKU_SIGNING_KEY` secret of the public repository's `release` environment. `KIKU_RELEASES` is the repository variable of that name, else `https://github.com/<repository>/releases`.

Before signing, `publish` derives the public key from `KIKU_SIGNING_KEY` (`release.py public-key`) and fails unless it equals `crates/core/update-key.txt`: copies built from that file refuse updates signed by any other key, and shipped copies cannot be recalled. After signing it verifies each manifest against the file's key (`scripts/ci/verify_manifest.py`, run with `uv`, which takes the signed text from `release.py`'s `signed_text`).

The snapshot `next` pushes goes on top of the commit of the highest release tag in the public repository (the public `main` tip when there is none), so the public `main` is always an ancestor of it, whether or not the last release finished moving `main`. A run publishes only this repository's `main` tip: a rerun of a superseded run publishes nothing, and a rerun after its own bump resumes from the bump. Re-running an old release run would move the Docker `latest` tag back, so a release run is re-run only to finish it.

Windows runners: when `ISCC.exe` is missing, `build` downloads Inno Setup 6.7.1 from its GitHub release (`jrsoftware/issrc`, tag `is-6_7_1`; `files.jrsoftware.org` holds only signatures), checks the file against the SHA-256 pinned in `scripts/ci/build_windows.py` (the digest GitHub records for it, not checked against a local download), and runs it silently with a 10 minute limit. Chocolatey is not used. `ISCC.exe` is an x86 program, run emulated on the ARM64 runner.

## Caches

`engine` restores the runtime (`third_party`) and cargo's registry and `target` (`target/cargo-registry`) with `actions/cache/restore` and saves them with `actions/cache/save` only on the release branch. `main.yml`'s `linux` job restores the same runtime and a cargo `target` of its own key (`cargo-dist-linux-x64-...`), since its build also makes the packing build; its `app` jobs restore the Windows runtime (`runtime-<arch>-...`: `third_party/sherpa-onnx`, `moonshine-src`, `moonshine-build`). Only the release branch saves them.

Cargo's caches are keyed on `Cargo.lock`, so each version bump saves new ones and GitHub evicts the oldest. In the public repository only `next` saves caches, so pull requests there, which read the default branch's caches, run cold.
