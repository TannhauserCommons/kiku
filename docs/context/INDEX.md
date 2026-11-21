# Context index

Where each kind of code lives. Facts that change with the code go in the page named here.

| Kind | Where | Page |
| --- | --- | --- |
| Everything that doesn't touch the OS: engines, models, settings, the MCP server | `crates/core/` | |
| The Windows app: capture, overlay, keys, tray | `crates/app/` | |
| Linux engine build: runtime, Moonshine, packaging, the build image | `scripts/fetch-runtime.sh`, `scripts/build-moonshine.sh`, `scripts/pack-linux.sh`, `scripts/linux-build.Dockerfile` | |
| Rust version | `rust-toolchain.toml` | [ci.md](ci.md) |
| CI: tasks, workflows, path map, tool pins | `mise.toml`, `mise.lock`, `scripts/ci/`, `.github/workflows/`, `.github/ci/` | [ci.md](ci.md) |
| The privacy gate and what it is run on | `scripts/privacy-gate.py`, `scripts/ci/privacy.py` | [ci.md](ci.md) |
| Feature switches | `switches.toml`, `crates/core/src/switches.rs` | |
