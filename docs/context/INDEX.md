# Context index

Curated pages for what is expensive to rediscover: boundaries, contracts, invariants. Code and tests outrank these pages.

`crates/core` (`kiku-core`) holds everything that doesn't touch the OS, plus the MCP server and GPU helper binaries; it must keep building on every OS, so OS calls go in `crates/app`. `crates/app` (`kiku`) is the Windows app: capture, the panel, keys and tray.

| Task | Read | Then open |
| --- | --- | --- |
| Speech engines, the helper process, a model on another computer, voices, the catalog, downloads, imports | [engines-and-models.md](engines-and-models.md) | `crates/core/src/engine.rs` |
| Settings, shared state, keys, feature switches, themes, the log | [settings-and-state.md](settings-and-state.md) | `crates/core/src/controls.rs`, `crates/core/src/settings.rs` |
| Word rules, voice actions, dictation, macros and their editor | [words-and-macros.md](words-and-macros.md) | `crates/core/src/words.rs`, `crates/core/src/macros/mod.rs` |
| Audio capture, discovery, captions, history, status, recording, the MCP server | [capture-and-transcripts.md](capture-and-transcripts.md) | `crates/app/src/main.rs`, `crates/core/src/captions.rs` |
| The panel, drawing, the settings window, captions mode, History, keys, clipboard | [panel-and-windows.md](panel-and-windows.md) | `crates/app/src/overlay/mod.rs` |
| Native builds, Moonshine, the Linux engine and Docker, installer, updates, releasing, agent loops | [build-and-release.md](build-and-release.md) | `scripts/build-release.ps1`, `scripts/pack-linux.sh` |
| The Linux engine build: runtime, Moonshine, packaging, the build image | [build-and-release.md](build-and-release.md) | `scripts/fetch-runtime.sh`, `scripts/build-moonshine.sh`, `scripts/pack-linux.sh`, `scripts/linux-build.Dockerfile` |
| Feature switches | [settings-and-state.md](settings-and-state.md) | `switches.toml`, `crates/core/src/switches.rs` |
| CI: tasks, workflows, path map, tool pins | [ci.md](ci.md) | `mise.toml`, `mise.lock`, `scripts/ci/`, `.github/workflows/`, `.github/ci/` |
| The privacy gate and what it is run on | [ci.md](ci.md) | `scripts/privacy-gate.py`, `scripts/ci/privacy.py` |
| Rust version | [ci.md](ci.md) | `rust-toolchain.toml` |
| General coding rules, commit messages | [coding-rules.md](coding-rules.md) | |
