# 12 Shaping verification record

Source checkout SHA: `3f63af24376e65ee0a0eab7f4c0d344d29e89787` (`kogen version` reported `3f63af24 (2026-10-06, uncommitted changes)` during verification).

Binary: `target/debug/kogen`, SHA-256 `9910a384e3dde4f666c2ad9417ebba6a5b5fe9ed170b8bef77e8cc3e7e43cb8b`.

`make check` passed: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.

The requested conformance command was:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen target/debug/kogen --case 'shape-01,shape-02,shape-03,shape-04,shape-05,shape-06,shape-07,shape-08,shape-09,shape-10,shape-11,shape-12,shape-13,shape-14,shape-15,shape-16,shape-17,shape-18,shape-19,shape-20,shape-21,shape-22,shape-23,shape-24,shape-25,shape-26,format-02,format-03,format-04,format-07,format-08,format-10,state-06,state-25' --keep -v --workdir /tmp/kogen-12-shaping-final --out /tmp/kogen-12-shaping-final/results.jsonl
```

Suite reported `v1.2+conformance-v1.1-2-gc190768`: 21 cases passed and 13 failed; no cases were skipped or unimplemented.

Passing IDs: `shape-01`, `shape-03`–`shape-13`, `shape-15`–`shape-18`, `shape-21`, `shape-23`–`shape-25`, `format-07`.

Failing IDs and observed blockers:

- `shape-02`, `shape-14`, `shape-19`, `shape-20`, `shape-22`: each invokes `intent shape ... --json`. Exit was 2 (expected 0); stdout was not JSON. Directly observed stdout bytes were `kogen intent shape: unknown option '--json'\n\nUsage: kogen intent shape <slug> <file|-> [options]\n\nShapes .kogen/intents/<slug>/intent.md and its acceptance test from the request in <file>\n(- reads stdin). Waits until the shaper finishes, with no time limit. Never approves.\nShaping defaults to gpt-6.1-sol at high effort. An explicit build.roles.shaper in\nproject or machine config overrides this default.\n\nOptions:\n  --project <checkout>  Project checkout (default: current directory)\n  --origin <repo>       Git repository holding Intent state and the target branch\n  --base <branch>       Target branch (project setting, origin HEAD, then current branch)\n`. This is the frozen v1.1 `--json` expectation versus fixed CLI §1.2. Do not add the flag. Request a versioned correction for these case expectations in the source repository; the supplied conformance checkout was left read-only.
- `shape-26`: diagnostic witness case only. It expected `Feasibility: PROVEN` / `UNPROVEN`; output was `Feasibility: not checked`. Witness remains off by default and has no public option.
- `state-06`, `state-25`, `format-02`, `format-03`, `format-04`, `format-08`, `format-10`: these invoke `intent approve`, whose handler is not present in this checkout. The observed response was `controller/internal_error: command handler is not available\n` (exit 70). They require the adjacent approval package before they can pass; this package is not complete until that dependency is integrated and these named cases rerun.

The isolated Quint `intent` model check passed all six hand scenarios. Generated model checks completed for 500 traces × 25 steps at each seed 17, 23 and 41; all generated invariants held. Exact production replay is blocked: `XSPEC_SLICE=/tmp/kogen-12-intent-xspec XSPEC_GOLDEN=/tmp/kogen-12-intent-hand-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py conform --only hand -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/12-shaping/target/debug/kogen-xspec intent` returned 0/6. The first divergence for all six was step 0 (`reset`): `adapter error: adapter gave no answer to {"op": "reset"} kogen-xspec: unsupported slice "intent"`. Therefore generated traces have model-generation results only; they are not counted as adapter replay passes. No Quint expectation or adapter code was changed.

Commands for the model checks (each seed used an isolated copy under `/tmp/kogen-12-intent-seed{17,23,41}`):

```sh
XSPEC_SLICE=/tmp/kogen-12-intent-xspec XSPEC_GOLDEN=/tmp/kogen-12-intent-hand-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py spec
XSPEC_SLICE=/tmp/kogen-12-intent-seed17 XSPEC_GOLDEN=/tmp/kogen-12-intent-seed17-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed 17
XSPEC_SLICE=/tmp/kogen-12-intent-seed23 XSPEC_GOLDEN=/tmp/kogen-12-intent-seed23-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed 23
XSPEC_SLICE=/tmp/kogen-12-intent-seed41 XSPEC_GOLDEN=/tmp/kogen-12-intent-seed41-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed 41
```
