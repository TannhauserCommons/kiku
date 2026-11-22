# Coding rules and commits

The general coding rules this project carried in AGENTS.md before it moved to the template layout, kept whole. The home rules and the project rules in AGENTS.md win where they differ.

## Commits

Commit messages carry no `Co-Authored-By` trailer or other AI attribution.

## Coding rules

* Prefer simple, surgical changes. Touch only what the task requires. Avoid speculative refactors, broad rewrites, and "while we're here" cleanups.
* Design for simplicity and readability. Choose the design that is easiest to understand and maintain. Code is read top to bottom: put high-level behavior first, hide lower-level details behind well-named helpers, and treat heavy branching as a signal to reconsider the design.
* Prefer modular, composable solutions. Avoid input-specific or case-specific hard-coding. Use existing extension points when they fit. If none fit, add a simple, focused helper, class, or plugin that cleanly captures the new behavior. Keep scope limited to known cases.
* Don't repeat yourself; keep a single source of truth. Consolidate repeated logic or intent with a shared helper, API, or abstraction when doing so keeps the design simpler. Avoid duplication that can drift out of sync.
* Comment cautiously. Comments should add context, not translate code into English. Prefer making the code self-explanatory first. Use comments only for non-obvious intent or constraints that remain unclear from the code. Apply this guidance to new comments only; do not rewrite or delete existing comments just for style. Keep comments to one or two lines. Keep docstrings to a one-line summary with optionally a short description plus only the `Args:`/`Returns:` entries whose meaning isn't obvious from the signature. Benchmark numbers and root-cause analysis belong in the PR description, not the source.
* Document public APIs. Public and higher-level APIs should have docstrings, including examples when useful. Internal helpers should usually be self-documenting through clear names and structure.
* Fix the bug cause, not the side effect. For bug fixes, find the root cause instead of patching for its side effect.
* Validate external input once. Check types and values at the interface boundary. Internal code can trust those checks and avoid redundant assertions.
* Remove dead code. Delete unused imports, unreachable branches, and obsolete helpers.
* Keep imports at the top of the file. Place all imports at module top in both source and test files so import errors surface at module load time rather than at runtime or during a specific test. Put an import inside a function only when there is a concrete reason: resolving a circular import that cannot be restructured, guarding an optional dependency (e.g., TensorRT-LLM, Megatron-Core), or deferring an unusually heavy import with explicit justification. Add a brief comment in those cases naming the reason.
* Keep tensor work on the GPU and avoid unnecessary CPU-GPU syncs. Reading metadata such as `tensor.shape` is fine. Avoid Python scalar extraction and operators such as `tensor.item()`, `float(tensor)`, or `min(tensor)` because they can trigger CPU-GPU syncs. Use PyTorch tensor ops such as `tensor.min()` by default, and only extract Python scalars when the CPU needs the value. Tensor-value-based Python branching can also break CUDA graphs.
* Develop with distributed processing in mind. Examples: Use `print_rank_0` or `warn_rank_0` when possible to avoid noisy logs. Guard shared side effects, such as file writes or shared state updates, against race conditions between ranks.
