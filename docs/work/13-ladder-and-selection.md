# 13 Ladder, repairs and candidate selection (L, 5–6 h)

Own `kogen-core::run::orchestration` and selector/auditor submodules. Depends on 07, 08 and 10. Implement started-Build terminal semantics, repair/fresh attempt/rung progression, best-candidate preservation, model `finish`, report fields and selected L recipes per spec §3.1, §3.4–3.8. Keep guaranteed green gate and CAS independent. Record where the frozen v1.1 rung policy disagrees with v1.2.

Acceptance: `build-04` through `build-08` inclusive, `build-31`, `build-39`, `build-44`, `ladder-01` through `ladder-36` inclusive, with `ladder-03`, `ladder-26`, `ladder-31` explicitly reported as version conflicts where their old policy disagrees. Quint: `orchestration` all 6 hand and `gate` all 5 hand scenarios plus 500 × 25 seeds 17, 23, 41 are diagnostic until L policies are promoted. Rerun the guaranteed `queue`, `rebase`, and `recovery` slices for regression; the green gate remains a black-box obligation.

## Implementation and verification record

The orchestration package now owns the started-Build state machine, repair accounting, fresh rung attempts, candidate selection, advisory scoring, the acceptance auditor, Build budget and report records. The default ladder is the v1.2 four-rung recipe; `finish` and budget cancellation request verification through explicit effects. The green receipt gate and CAS landing code were left independent.

Source bundle SHA-256: `84d198d5b9df7040db185f53577af1da74b87cbc42acbefcfd67317035405479` (the sorted changed/added `crates/` paths, NUL, file bytes, NUL). Spec source SHA: `1118f7fc0dbb042af8c8de2ffd1b85768cf9e2a0`. Conformance source SHA: `c1907685e52b9ba965f540b6cc162b489d4f4334`.

Final binary SHA-256:

- `target/debug/kogen`: `79dfc79a13600a27681a5af4a3cef31dd65249847cbaa330ed34ecb835f6648b`
- `target/debug/kogen-xspec`: `f13cdf3be7e0c75d077977ef6bbb00c251b26b1ec12dbde31736545e53c7c73d`

`make check` passed with 141 `kogen-core` tests and 2 `kogen-xspec` tests. Focused package command `cargo test -p kogen-core run::orchestration --no-fail-fast` passed 32/32 tests. Build command `cargo build --workspace` passed.

```sh
env HOME=/tmp/kogen-13-home CARGO_HOME=/Users/almirsarajcic/.cargo RUSTUP_HOME=/Users/almirsarajcic/.rustup make check
env HOME=/tmp/kogen-13-home CARGO_HOME=/Users/almirsarajcic/.cargo RUSTUP_HOME=/Users/almirsarajcic/.rustup cargo test -p kogen-core run::orchestration --no-fail-fast
```

### Quint

The read-only Quint slices were copied under `/tmp/kogen-13-traces-20261007`; every command used `HOME=/tmp/kogen-13-home`. Model hand checks and full adapter comparisons passed: `orchestration` 6/6 hand scenarios and 76 steps; `gate` 5/5 and 29 steps.

Generated traces used `--traces 500 --steps 25` for each seed 17, 23 and 41. `gate` matched all 500 traces and 12,500 steps at every seed, with no divergence. `orchestration` matched 464/500 traces (12,277 processed steps) for seeds 17 and 23, and 463/500 (12,255 steps) for seed 41. Every first divergence was a `Budget` event: the older Quint transition selects immediately, while v1.2 §3.4 cancels the in-flight stage, verifies and snapshots its current tree, then proceeds to selection. First divergence: seed 17/23 `g0017` step 15; seed 41 `g0017` step 14. Quint wanted `phase=done,status=failed,reason=best_candidate,claim=false,exit=1,journal=[started,plan,rung_started,selection,finished]`; the implementation remained `phase=setup,status=running,reason="",claim=true,exit=0,journal=[started,plan,rung_started]` while requesting candidate verification. These slices remain diagnostic under the current classification.

Exact generated command pattern:

```sh
for SLICE in orchestration gate; do
  for SEED in 17 23 41; do
    env HOME=/tmp/kogen-13-home XSPEC_SLICE="/tmp/kogen-13-traces-20261007/${SLICE}-${SEED}" XSPEC_GOLDEN="/tmp/kogen-13-traces-20261007/${SLICE}-golden-${SEED}" python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed "$SEED"
    env HOME=/tmp/kogen-13-home XSPEC_SLICE="/tmp/kogen-13-traces-20261007/${SLICE}-${SEED}" XSPEC_GOLDEN="/tmp/kogen-13-traces-20261007/${SLICE}-golden-${SEED}" python3 ../../kogen-spec/quint/prototype/harness/xspec.py conform --only gen -- "$PWD/target/debug/kogen-xspec" "$SLICE"
  done
done
```

Regression slices passed with exact full observations: `queue` 9/9 hand plus 500/500 traces for each seed; `rebase` 7/7 plus 500/500 each seed; `recovery` 5/5 plus 500/500 each seed. Each generated slice used 12,500 steps per seed. First divergence: none.

### Public conformance cases

Exact case command, with a temporary HOME and the suite's fake provider endpoints:

```sh
env HOME=/tmp/kogen-13-home ../../kogen-conformance/bin/kogen-conformance run --kogen "$PWD/target/debug/kogen" --case 'build-04,build-05,build-06,build-07,build-08,build-31,build-39,build-44,ladder-01,ladder-02,ladder-03,ladder-04,ladder-05,ladder-06,ladder-07,ladder-08,ladder-09,ladder-10,ladder-11,ladder-12,ladder-13,ladder-14,ladder-15,ladder-16,ladder-17,ladder-18,ladder-19,ladder-20,ladder-21,ladder-22,ladder-23,ladder-24,ladder-25,ladder-26,ladder-27,ladder-28,ladder-29,ladder-30,ladder-31,ladder-32,ladder-33,ladder-34,ladder-35,ladder-36' --jobs 1 --workdir /tmp/kogen-13-conformance-work-final --out /tmp/kogen-13-conformance-results-final.jsonl -v
```

Passed: none. Failed: `build-04`, `build-05`, `build-06`, `build-07`, `build-08`, `build-31`, `build-39`, `build-44`, `ladder-01`, `ladder-02`, `ladder-03`, `ladder-04`, `ladder-05`, `ladder-06`, `ladder-07`, `ladder-08`, `ladder-09`, `ladder-10`, `ladder-11`, `ladder-12`, `ladder-13`, `ladder-14`, `ladder-15`, `ladder-16`, `ladder-17`, `ladder-18`, `ladder-19`, `ladder-20`, `ladder-21`, `ladder-22`, `ladder-23`, `ladder-24`, `ladder-25`, `ladder-26`, `ladder-27`, `ladder-28`, `ladder-29`, `ladder-30`, `ladder-31`, `ladder-32`, `ladder-33`, `ladder-34`, `ladder-35`, `ladder-36`. No skips, errors or unimplemented cases.

Forty-two cases stop at `kogen queue start`, before any provider request: exact observed stdout bytes are `controller/internal_error: command handler is not available\n`. `ladder-32` and `ladder-33` stop earlier at intent shaping: both expected `Feasibility: PROVEN\n` but the current command printed `Feasibility: not checked\n`. The missing queue-to-Build dispatcher and witness-shaping integration belong to adjacent packages; the selected Build behavior cannot be black-box accepted until those dependencies are integrated.

### Frozen v1.1 conflicts and v1.2 rules

| Case | Superseded v1.1 assertion | v1.2 rule and matching local test | Runtime result |
|---|---|---|---|
| `ladder-03` | Hard difficulty starts only R2 and gives it the plan. | §3.1 starts R1 and R2 in parallel by default. `hard_plan_starts_two_rungs_and_red_pair_advances_to_three` passed. | Step 3 stopped before the policy assertion: expected exit 0, observed exit 70 and `controller/internal_error: command handler is not available\n`. |
| `ladder-26` | Rung walls are 20/12/12 minutes. | §3.1 has one 60-minute Build wall and caps each stage at the remaining budget or 30 minutes. `build_and_stage_share_one_wall_clock` passed. | Step 3 expected exit 0; observed exit 70 and `controller/internal_error: command handler is not available\n`. |
| `ladder-31` | R4 is disabled unless `experimental_r4` is set. | §3.1 includes experimental `raw-request` R4 in the four-rung default. `default_ladder_has_the_v12_four_rung_recipe` passed. | Step 3 expected exit 1; observed exit 70 and `controller/internal_error: command handler is not available\n`. |

The frozen conformance tree has no matching v1.2 ladder case IDs; the matching rules are in `spec/03-build.md` §3.1 and the local tests above. Request a versioned correction in the source repository that retains the frozen assertions as superseded cases and adds v1.2 replacements. The supplied conformance checkout was left read-only. These cases were run and failed at the stated dependency boundary; the old policy assertions were not reached and are not counted as passes. The package is not claimed fully accepted until the required black-box cases pass after the adjacent dispatcher and witness dependencies are integrated.
