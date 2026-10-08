# Conformance and validation

**Status: v1.3, 278/278 cases passed.** The full v1.3 suite also reports 619/619 passing instances, with no failures, errors, or skips.

Validation for this publication candidate includes:

- GIT_CONFIG_GLOBAL=/dev/null make check for formatting, Clippy, and workspace tests.
- cargo build --locked from a clean clone with a fresh Cargo home.
- cargo check --target x86_64-unknown-linux-gnu.
- The complete versioned v1.3 conformance suite against the Kogen executable built from this branch.

The v1.3 suite drives the CLI and checks output, files, Git refs, run records, and fake-provider requests. It is separate from Rust unit tests and exact Quint replay. The conformance result does not measure live provider availability or cache performance. Live multi-turn warm-cache behavior remains unverified.

Runtime confinement is implemented with sandbox-exec on macOS and bwrap on Linux when available. The runner has an explicitly reported unconfined fallback. The Linux cross-target check validates compilation only; it does not exercise Linux process confinement.
