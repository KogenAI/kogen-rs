# 08 Git CAS landing (G, 5–6 h)

Own landing submodule of `kogen-core::git` and the `kogen-xspec` `rebase` dispatch. Depends on 06 and 07. Implement fresh clone/private index, candidate commit with one expected parent, durable landing record, incoming ref, base compare-and-swap, moved-base re-gate/integration repair, checked-out-base handling and cleanup. No hook or AI trailer. Use spec §2.5.3–2.5.4, §3.9–3.10 and §5.4.

Acceptance: `build-02`, `build-03`, `build-22`, `build-23`, `build-24`, `build-25`, `build-26`, `build-27`, `build-28`, `build-29`, `build-30`, `build-37`, `build-43`, `custody-09`, `custody-10`, `v1.2-06-crash-after-base-cas`. Quint: `rebase` all 7 hand and `recovery` all 5 hand scenarios, each at 500 × 25 for seeds 17, 23, 41. Run effectful CAS traces against a temporary bare origin; assert record-before-CAS, exact parent/tree, re-gate after tip movement, and landed after cleanup crash.

## Implementation and verification

Implementation source commit: `d54c468bd0551911b9b662ec220160d7dcd956b8`.

Binary SHA-256:

- `target/debug/kogen`: `795f54a24ff1b6259ed274fb754a7dee3851d445e663ac112c697aabb770a30b`
- `target/debug/kogen-xspec`: `5ce8d53d2ef3e658b001c2f2e450dc6eb4fb045e925c7bab2a80d64c76f08ee7`

`make check` passed under temporary `HOME=/tmp/kogen-08-home` with 109 core tests and 2 xspec tests. The package-specific effect command also passed all 6 tests, each using a temporary bare origin:

```sh
env HOME=/tmp/kogen-08-home RUSTUP_HOME=/Users/almirsarajcic/.rustup CARGO_HOME=/Users/almirsarajcic/.cargo make check
env HOME=/tmp/kogen-08-home RUSTUP_HOME=/Users/almirsarajcic/.rustup CARGO_HOME=/Users/almirsarajcic/.cargo cargo test -p kogen-core git::landing::tests --no-fail-fast
```

The effects assert: the durable run snapshot contains the candidate before incoming publication and base CAS; the origin still points to the expected parent immediately before CAS; the landing commit has exactly that one parent and the verified tree; a moved tip is fetched, rebased, repaired and re-gated before a new candidate is recorded; the lock retry waits one second; clean linked worktrees advance while dirty ones are preserved and warned; and recovery after a crash immediately after base CAS marks the run landed and removes the incoming ref and claim. The publication test also verifies the exact commit message and configured identity, and proves the workspace hook and filter did not run.

Quint used temporary copies of the read-only slices at `/tmp/kogen-08-quint-run/{rebase,recovery}`. Model hand checks passed 7/7 `rebase` and 5/5 `recovery`; exact adapter hand replays passed the same counts. Each slice then passed 500 generated traces × 25 steps at seeds 17, 23 and 41, with all 500 traces and 12,500 events agreeing with full observations for each seed. First divergence: none.

The generated command pattern was:

```sh
env HOME=/tmp/kogen-08-home XSPEC_SLICE=/tmp/kogen-08-quint-run/rebase XSPEC_GOLDEN=/tmp/kogen-08-quint-run/rebase-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed <17|23|41>
env HOME=/tmp/kogen-08-home XSPEC_SLICE=/tmp/kogen-08-quint-run/rebase XSPEC_GOLDEN=/tmp/kogen-08-quint-run/rebase-golden python3 ../../kogen-spec/quint/prototype/harness/xspec.py conform --only gen -- "$PWD/target/debug/kogen-xspec" rebase
```

The same two commands were run with `recovery` in both slice and adapter positions. Hand adapter commands used `conform --only hand`; model checks used `spec`.

The exact selected conformance command was:

```sh
env HOME=/tmp/kogen-08-home ../../kogen-conformance/bin/kogen-conformance run --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/08-cas-landing/target/debug/kogen --case 'build-02,build-03,build-22,build-23,build-24,build-25,build-26,build-27,build-28,build-29,build-30,build-37,build-43,custody-09,custody-10,v1.2-06-crash-after-base-cas' --jobs 1 --workdir /tmp/kogen-08-conformance-work --out /tmp/kogen-08-conformance-results.jsonl -v
```

Result: 1/16 cases passed (17 total instances); no skips, projections or unimplemented cases. Pass: `build-37`. Fail: `build-02`, `build-03`, `build-22`, `build-23`, `build-24`, `build-25`, `build-26`, `build-27`, `build-28`, `build-29`, `build-30`, `build-43`, `custody-09`, `custody-10`, and `v1.2-06-crash-after-base-cas`.

Fourteen failing v1.1 cases (`build-02`, `build-03`, `build-22`, `build-23`, `build-24`, `build-25`, `build-26`, `build-27`, both `build-28` instances, `build-29`, `build-30`, `build-43`, `custody-09`, `custody-10`) stop at `kogen queue start`: the case reports `cannot evaluate the expectation: no run found for greet`, and observed stdout is exactly `controller/internal_error: command handler is not available\n`. Steps were respectively 4, 3, 5, 5, 3, 3, 6, 4, 3 (both instances), 4, 4, 8, 3 and 4. They do not reach package 08’s landing path. `build-37` is the synthetic landed-run recovery case and passes.

`v1.2-06-crash-after-base-cas` fails at step 6 with the exact assertion `condition not reached within 60.0s: {"file_exists": "{case}/post-cas"}`; queue start never invokes a Build, so the crash marker is not produced. The v1.1 `build-25` integration-repair case also stops at the queue-start boundary and does not reach its repair assertion. No v1.1/v1.2 assertion conflict was observed, so no conformance correction is requested. The remaining named black-box cases require the queue-start Build dispatcher from an adjacent package before package 08 can be marked fully accepted.
