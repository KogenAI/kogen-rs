# 07 Acceptance gate and protection (G kernel, 5–6 h)

Own `kogen-core::gate` except the package 03 ledger port; build the private tree/index verification interface. Depends on 03 and 04. Install the approved test in a workspace, evaluate each tagged A item, run configured checks against base and candidate, compare tree identity, restore protected files, and refuse red/unverified candidates. Use spec §2.4–2.5, §3.7–3.9.4. Keep auditor demotion and selector ranking out of this green gate kernel.

Acceptance: `build-09`, `build-10`, `build-11`, `build-12`, `build-13`, `build-14`, `build-15`, `build-16`, `build-17`, `build-18`, `build-19`, `build-20`, `build-21`, `build-23`, `build-40`, `approval-15`, `approval-16`, `custody-09`. Quint: full `gate` (5 hand and 500 × 25 seeds 17, 23, 41) is diagnostic because it models L demotion/ranking; there is no separate guaranteed gate slice yet. Black-box cases and unit tests prove the green/no-change/failed-check kernel. No verification bypass for a candidate with a model `finish` call.

## Verification record

Implementation source commit: `4cf3c7c` (`Implement acceptance gate and workspace protection`). Spec source SHA: `1118f7fc0dbb042af8c8de2ffd1b85768cf9e2a0`. Conformance source SHA: `c1907685e52b9ba965f540b6cc162b489d4f4334`.

`make check` passed: formatting, clippy with warnings denied, and all workspace tests (76 passed). The focused gate run `cargo test -p kogen-core gate --no-fail-fast` passed all 19 gate tests. `cargo build --workspace` succeeded. Built binary SHA-256:

| Binary | SHA-256 |
|---|---|
| `target/debug/kogen` | `58c45b703f9b54e1014e853f8947971f9a3ee2062ae1aeed03438b77aaed06f8` |
| `target/debug/kogen-xspec` | `c54fb8cdc8e453b8dba1b945a389adf40b943d544dddf2185b8662caaf451a4a` |

The conformance runner creates a temporary HOME for each case and injects fake provider endpoints. Exact named-case command:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen "$PWD/target/debug/kogen" --case 'build-09,build-10,build-11,build-12,build-13,build-14,build-15,build-16,build-17,build-18,build-19,build-20,build-21,build-23,build-40,approval-15,approval-16,custody-09' --jobs 1 -v --out /tmp/kogen-07-gate-conformance.jsonl
```

Passed: `approval-15`, `approval-16`. Failed: `build-09`, `build-10`, `build-11`, `build-12`, `build-13`, `build-14`, `build-15`, `build-16`, `build-17`, `build-18`, `build-19`, `build-20`, `build-21`, `build-23`, `build-40`, `custody-09`. No skipped, unimplemented, or harness-error cases. Each failure reaches `queue start`, whose exact stdout bytes are `controller/internal_error: command handler is not available\n`; the runner then reports `cannot evaluate the expectation: no run found for greet`. `build-23` reaches this at step 5; the others fail at step 3. Queue/Build dispatch is absent from the dependency source in this worktree, as package 04's verification note also records. These frozen cases are not superseded v1.1 assertions and this is not a spec conflict. They remain failed and the package is not claimed fully accepted until the queue/Build dependency is integrated and the named cases pass.

Quint diagnostic commands:

```sh
XSPEC_SLICE=../slices/gate python3 harness/xspec.py spec
XSPEC_SLICE=../slices/gate python3 harness/xspec.py gen --traces 500 --steps 25 --seed 17
XSPEC_SLICE=../slices/gate python3 harness/xspec.py conform --only gen -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/07-gate-and-protection/target/debug/kogen-xspec gate
XSPEC_SLICE=../slices/gate python3 harness/xspec.py gen --traces 500 --steps 25 --seed 23
XSPEC_SLICE=../slices/gate python3 harness/xspec.py conform --only gen -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/07-gate-and-protection/target/debug/kogen-xspec gate
XSPEC_SLICE=../slices/gate python3 harness/xspec.py gen --traces 500 --steps 25 --seed 41
XSPEC_SLICE=../slices/gate python3 harness/xspec.py conform --only gen -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/07-gate-and-protection/target/debug/kogen-xspec gate
```

The Quint model's hand expectations passed 5/5 and each generated matrix passed model invariants for 12,500 events. Exact Rust adapter comparisons were diagnostic only: `kogen-xspec gate` is unsupported, so hand comparison was 0/5 and each seed was 0/500 traces, 0 events observed. First divergence for each matrix: `g0000`, step 0, reset; adapter stderr was `kogen-xspec: unsupported slice "gate"`. This full model contains the L-level auditor and selector policy, so no such behavior was added to the green gate kernel and no trace result is counted as a gate pass.
