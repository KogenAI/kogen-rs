# Integration pass: Rust rewrite

Date: 2026-10-07. Worktree branch: `krs/integration-1`. Source base: `fae2133d12a93361291951f6f9bcd07fc716f290`.

This report covers guaranteed packages 01–11, the read-only v1.1 black-box profiles, the six v1.2 cases, and every guaranteed Quint slice. Package 13 (ladder) was run separately as LIKELY; its implementation area was not changed. Neither the conformance repository nor the spec repository was modified.

## Build and validation

Built both release binaries from the same source revision:

```sh
cargo build --release --bin kogen --bin kogen-xspec
make check
```

`make check` passed formatting, clippy with warnings denied, and 116 workspace tests (1 CLI, 113 core, 2 xspec). Release binary SHA-256:

| Binary | SHA-256 |
|---|---|
| `target/release/kogen` | `dbf0a71a8e1eac6b74660fe1939ee2411bef6255350362c08e90f04571539f22` |
| `target/release/kogen-xspec` | `6b15b26ed6f0231e4c37114a0cdf9f6f29570e26195ccf76d2cf61f4e77ae8ee` |

Conformance source: `kogen-conformance` branch `v1.2`, commit `c190768` (version string `v1.2+conformance-v1.1-2-gc190768`, read-only). Spec source: `kogen-spec` branch `main`, commit `1118f7f` (read-only). The suite runner used isolated temporary HOME/work directories and time scale 0.01.

Commands used:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen target/release/kogen --profile cli,state,approval,shape,build,provider,custody,v1.2 --jobs 8 --time-scale 0.01 --workdir "$RUN/work" --out "$RUN/results.jsonl" --keep --quiet
../../kogen-conformance/bin/kogen-conformance run --kogen target/release/kogen --profile ladder --jobs 8 --time-scale 0.01 --workdir "$LADDER_RUN/work" --out "$LADDER_RUN/results.jsonl" --keep --quiet
```

For Quint, each of the eight slice specs was checked with `xspec.py spec`; generated traces were produced with `xspec.py gen --traces 500 --steps 25 --seed 17|23|41`; then exact `xspec.py conform` ran with `target/release/kogen-xspec <slice>`. Conformance compared full reset and post-event observations with no projection. A single long-lived adapter process per slice replayed the hand goldens and all 1,500 generated traces; generated trace names were seed-prefixed in temporary gold folders without changing events or observations.

## Black-box profile results

| Profile | Case pass | Cases | Instance pass | Instances |
|---|---:|---:|---:|---:|
| `cli` | 8 | 30 | 61 | 156 |
| `state` | 17 | 30 | 66 | 80 |
| `approval` | 22 | 24 | 27 | 29 |
| `shape` | 20 | 26 | 23 | 30 |
| `build` | 1 | 44 | 1 | 45 |
| `provider` | 3 | 26 | 12 | 36 |
| `custody` | 3 | 10 | 3 | 11 |
| `v1.2` | 2 | 6 | 22 | 26 |
| **Total** | **76** | **196** | **215** | **413** |

The runner reported zero errors, skips, and unimplemented cases. Across 137 retained fake-request snapshots it recorded 175 fake-provider requests; all 175 had a matched step (0 unmatched). The provider profile contributed 64 requests; the separate ladder run contributed 8 more, also with 0 unmatched.

### Per-case results and first divergence

Each count is passing instances / total instances. “First divergence” is the first failed runner step and its first assertion; successful cases show `—`.

#### `cli`

| Case | Pass / total | First divergence |
|---|---:|---|
| `cli-01` | 0/3 | step 1 (run kogen): stdout differs at char 249 |
| `cli-02` | 0/36 | step 1 (run kogen help help): exit 2, expected 0 |
| `cli-03` | 0/2 | step 1 (run kogen frob): stdout differs at char 280 |
| `cli-04` | 0/6 | step 1 (run kogen intent frob): stdout differs at char 297 |
| `cli-05` | 5/6 | step 1 (run kogen status --bogus): stdout differs at char 224 |
| `cli-06` | 2/7 | step 1 (run kogen intent shape): stdout differs at char 265 |
| `cli-07` | 4/6 | step 1 (run kogen status greet other): stdout differs at char 227 |
| `cli-08` | 3/5 | step 1 (run kogen status --project): stdout differs at char 223 |
| `cli-09` | 2/5 | step 1 (run kogen status --json=true): stdout differs at char 221 |
| `cli-10` | 5/5 | — |
| `cli-11` | 0/4 | step 1 (run kogen provider login github): stdout differs at char 67 |
| `cli-12` | 0/2 | step 1 (run kogen status --watch --json): stdout differs at char 236 |
| `cli-13` | 2/3 | step 1 (run kogen status -- --json): stdout differs at char 265 |
| `cli-14` | 0/2 | step 1 (run kogen --project x status): stdout differs at char 285 |
| `cli-15` | 1/1 | — |
| `cli-16` | 2/3 | step 1 (run kogen status -x): stdout differs at char 219 |
| `cli-17` | 0/3 | step 1 (run kogen help frob): stdout differs at char 12 |
| `cli-18` | 15/15 | — |
| `cli-19` | 3/3 | — |
| `cli-20` | 1/1 | — |
| `cli-21` | 4/7 | step 1 (run kogen status ab): stdout differs at char 265 |
| `cli-22` | 10/10 | — |
| `cli-23` | 1/1 | — |
| `cli-24` | 0/14 | step 1 (run kogen status greet --help): exit 2, expected 0 |
| `cli-25` | 0/1 | step 28 (run kogen status): stdout has 22 lines, expected 21 |
| `cli-26` | 0/1 | step 16 (run kogen status fail-a): stdout has 4 lines, expected 5 |
| `cli-27` | 0/1 | step 4 (wait_until): wait_until timed out (fake_requests=1) |
| `cli-28` | 1/1 | — |
| `cli-29` | 0/1 | step 3 (run kogen queue start): wait_until timed out (fake_requests=1) |
| `cli-30` | 0/1 | step 3 (run kogen queue start): exit 70, expected 4 |

#### `state`

| Case | Pass / total | First divergence |
|---|---:|---|
| `state-01` | 20/20 | — |
| `state-02` | 0/1 | step 2 (run kogen status): stdout matched a forbidden regex |
| `state-03` | 1/1 | — |
| `state-04` | 16/16 | — |
| `state-05` | 14/14 | — |
| `state-06` | 0/1 | step 2 (run kogen intent approve greet): stdout missing lint_item_too_long warning |
| `state-07` | 1/1 | — |
| `state-08` | 1/1 | — |
| `state-09` | 1/1 | — |
| `state-10` | 0/1 | step 4 (run kogen queue start): stdout regex assertion failed |
| `state-11` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `state-12` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `state-13` | 1/1 | — |
| `state-14` | 0/2 | step 2 (run kogen provider list): stdout length 43, expected 23 |
| `state-15` | 0/1 | step 4 (run kogen queue start): exit 70, expected 0 |
| `state-16` | 3/3 | — |
| `state-17` | 1/1 | — |
| `state-18` | 0/1 | step 4 (wait_until): wait_until timed out (fake_requests=1) |
| `state-19` | 1/1 | — |
| `state-20` | 0/1 | step 5 (run kogen status): stdout has 4 lines, expected 3 |
| `state-21` | 1/1 | — |
| `state-22` | 0/1 | step 9 (run kogen status): stdout has 6 lines, expected 5 |
| `state-23` | 0/1 | step 8 (run kogen queue start): exit 70, expected 0 |
| `state-24` | 1/1 | — |
| `state-25` | 1/1 | — |
| `state-26` | 0/1 | step 4 (run kogen queue start): exit 70, expected 1 |
| `state-27` | 0/1 | step 13 (run kogen queue start): exit 70, expected 0 |
| `state-28` | 1/1 | — |
| `state-29` | 1/1 | — |
| `state-30` | 1/1 | — |

#### `approval`

| Case | Pass / total | First divergence |
|---|---:|---|
| `approval-01` | 1/1 | — |
| `approval-02` | 3/3 | — |
| `approval-03` | 1/1 | — |
| `approval-04` | 1/1 | — |
| `approval-05` | 1/1 | — |
| `approval-06` | 1/1 | — |
| `approval-07` | 1/1 | — |
| `approval-08` | 1/1 | — |
| `approval-09` | 1/1 | — |
| `approval-10` | 1/1 | — |
| `approval-11` | 1/1 | — |
| `approval-12` | 1/1 | — |
| `approval-13` | 1/1 | — |
| `approval-14` | 1/1 | — |
| `approval-15` | 1/1 | — |
| `approval-16` | 1/1 | — |
| `approval-17` | 1/1 | — |
| `approval-18` | 0/1 | step 5 (run kogen status): stdout has 4 lines, expected 3 |
| `approval-19` | 1/1 | — |
| `approval-20` | 1/1 | — |
| `approval-21` | 1/1 | — |
| `approval-22` | 1/1 | — |
| `approval-23` | 4/4 | — |
| `approval-24` | 0/1 | step 7 (wait_until): wait_until timed out (fake_requests=1) |

#### `shape`

| Case | Pass / total | First divergence |
|---|---:|---|
| `shape-01` | 1/1 | — |
| `shape-02` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md --json): exit 2, expected 0 |
| `shape-03` | 1/1 | — |
| `shape-04` | 1/1 | — |
| `shape-05` | 2/2 | — |
| `shape-06` | 1/1 | — |
| `shape-07` | 1/1 | — |
| `shape-08` | 1/1 | — |
| `shape-09` | 1/1 | — |
| `shape-10` | 1/1 | — |
| `shape-11` | 1/1 | — |
| `shape-12` | 1/1 | — |
| `shape-13` | 1/1 | — |
| `shape-14` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md --json): exit 2, expected 0 |
| `shape-15` | 1/1 | — |
| `shape-16` | 2/2 | — |
| `shape-17` | 1/1 | — |
| `shape-18` | 1/1 | — |
| `shape-19` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md --json): exit 2, expected 0 |
| `shape-20` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md --json): exit 2, expected 0 |
| `shape-21` | 1/1 | — |
| `shape-22` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md --json): exit 2, expected 0 |
| `shape-23` | 2/2 | — |
| `shape-24` | 1/1 | — |
| `shape-25` | 1/1 | — |
| `shape-26` | 0/2 | step 2 (run kogen intent shape greet {case}/request.md): stdout regex assertion failed |

#### `build`

| Case | Pass / total | First divergence |
|---|---:|---|
| `build-01` | 0/1 | step 1 (run kogen queue start): exit 70, expected 0 |
| `build-02` | 0/1 | step 4 (run kogen queue start): no run found for greet |
| `build-03` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-04` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-05` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-06` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-07` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-08` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-09` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-10` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-11` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-12` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-13` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-14` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-15` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-16` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-17` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-18` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-19` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-20` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-21` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-22` | 0/1 | step 5 (run kogen queue start): no run found for greet |
| `build-23` | 0/1 | step 5 (run kogen queue start): no run found for greet |
| `build-24` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-25` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-26` | 0/1 | step 6 (run kogen queue start): no run found for greet |
| `build-27` | 0/1 | step 4 (run kogen queue start): no run found for greet |
| `build-28` | 0/2 | step 3 (run kogen queue start): no run found for greet |
| `build-29` | 0/1 | step 4 (run kogen queue start): no run found for greet |
| `build-30` | 0/1 | step 4 (run kogen queue start): no run found for greet |
| `build-31` | 0/1 | step 7 (run kogen queue start): exit 70, expected 1 |
| `build-32` | 0/1 | step 4 (wait_until): wait_until timed out (fake_requests=1) |
| `build-33` | 0/1 | step 7 (wait_until): wait_until timed out (fake_requests=1) |
| `build-34` | 0/1 | step 3 (run kogen queue start --detach): exit 70, expected 0 |
| `build-35` | 0/1 | step 2 (run kogen queue start): exit 70, expected 0 |
| `build-36` | 0/1 | step 4 (wait_until): wait_until timed out (fake_requests=1) |
| `build-37` | 1/1 | — |
| `build-38` | 0/1 | step 3 (run kogen queue start): wait_until timed out (fake_requests=1) |
| `build-39` | 0/1 | step 4 (run kogen queue start): no run found for greet |
| `build-40` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `build-41` | 0/1 | step 5 (wait_until): wait_until timed out (fake_requests=1) |
| `build-42` | 0/1 | step 4 (run kogen queue start --base other): exit 70, expected 0 |
| `build-43` | 0/1 | step 8 (run kogen queue start): no run found for greet |
| `build-44` | 0/1 | step 3 (run kogen queue start): no run found for greet |

#### `provider`

| Case | Pass / total | First divergence |
|---|---:|---|
| `provider-01` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-03` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `provider-04` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-05` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-06` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-07` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-08` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-09` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-11` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-12` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-13` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-14` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-15` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-16` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-17` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-18` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-19` | 0/1 | step 3 (run kogen queue start): exit 70, expected 4 |
| `provider-20` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-23` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-24` | 10/10 | — |
| `provider-25` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `provider-26` | 1/1 | — |
| `provider-02` | 0/1 | step 4 (run kogen queue start): exit 70, expected 0 |
| `provider-10` | 0/2 | step 4 (run kogen queue start): exit 70, expected 4 |
| `provider-21` | 0/1 | step 1 (run kogen provider list): stdout length 43, expected 23 |
| `provider-22` | 1/1 | — |

#### `custody`

| Case | Pass / total | First divergence |
|---|---:|---|
| `custody-01` | 1/1 | — |
| `custody-02` | 1/1 | — |
| `custody-03` | 1/1 | — |
| `custody-04` | 0/1 | step 4 (wait_until): wait_until timed out (expected file was not created) |
| `custody-05` | 0/2 | step 3 (run kogen queue start): wait_until timed out (fake_requests=1) |
| `custody-06` | 0/1 | step 6 (run kogen queue start): exit 70, expected 0 |
| `custody-07` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `custody-08` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `custody-09` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `custody-10` | 0/1 | step 4 (run kogen queue start): no run found for greet |

#### `v1.2`

| Case | Pass / total | First divergence |
|---|---:|---|
| `v1.2-01-fixed-cli-help-and-grok` | 21/21 | — |
| `v1.2-02-approval-hash-intent-and-test-bytes` | 1/1 | — |
| `v1.2-03-consecutive-request-byte-prefix` | 0/1 | step 5 (wait_until): wait_until timed out (fake_requests=4) |
| `v1.2-04-cache-key-session-headers` | 0/1 | step 5 (wait_until): wait_until timed out (fake_requests=4) |
| `v1.2-05-missing-usage` | 0/1 | step 5 (wait_until): wait_until timed out (fake_requests=4) |
| `v1.2-06-crash-after-base-cas` | 0/1 | step 6 (wait_until): wait_until timed out (expected file was not created) |

### LIKELY ladder profile (separate run)

`ladder`: 0/36 cases, 0/36 instances. This is reported separately from guaranteed acceptance. The current first divergence for almost all cases is queue start returning exit 70 with `controller/internal_error: command handler is not available`; `ladder-32` and `ladder-33` instead fail their `Feasibility: PROVEN` witness expectation. The ladder run made 8 matched fake-provider requests and had no unmatched requests. Package 13 source was not touched.

| Case | Pass / total | First divergence |
|---|---:|---|
| `ladder-01` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-02` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-03` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-04` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-05` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-06` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-07` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-08` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-09` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-10` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-11` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-12` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-13` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-14` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-15` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-16` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-17` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-18` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-19` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-20` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-21` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-22` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-23` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-24` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-25` | 0/1 | step 3 (run kogen queue start): no run found for greet |
| `ladder-26` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-27` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-28` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-29` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-30` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-31` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-32` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md): stdout missing Feasibility: PROVEN |
| `ladder-33` | 0/1 | step 2 (run kogen intent shape greet {case}/request.md): stdout missing Feasibility: PROVEN |
| `ladder-34` | 0/1 | step 3 (run kogen queue start): exit 70, expected 1 |
| `ladder-35` | 0/1 | step 3 (run kogen queue start): exit 70, expected 0 |
| `ladder-36` | 0/1 | step 3 (run kogen queue start): exit 70, expected 4 |

## Guaranteed Quint slices

All hand scenarios passed the Quint model checks (`xspec.py spec`). Every generated run checked invariants on 500 traces × 25 steps at each seed 17, 23, and 41 (12,500 events per seed); all generated model runs held their invariants. Adapter conformance used exact full observations through `kogen-xspec`.

| Slice | Model hand | Adapter hand | Adapter generated | Adapter total | First divergent event |
|---|---:|---:|---:|---:|---|
| `approve` | 18/18 | 3/18 | 0/1,500 | 3/1,518 | Hand `01-card-then-approve`, step 1 `Approve`: expected `sha8=aaaa1111`, got `eab21f39`. Generated `g0000`: seed 17 step 2 expected `environment/setup_failed`, got `intent/hash_mismatch`; seed 23 step 1 expected `intent/approval_identity_unavailable`, got `intent/hash_mismatch`; seed 41 step 1 expected `check/acceptance_check_failed`, got `intent/hash_mismatch`. |
| `intent` | 6/6 | 1/6 | 1,062/1,500 | 1,063/1,506 | Hand `01-shape-then-approve`, step 2 `Approve`: expected `shown=abcd1234`, got actual source hash `eab21f3977c38080f34317c18eaa85177705a2c43a3c20020e5808bce4e42cac`. Generated `g0000`: seed 17 step 13, seed 23 step 12, seed 41 step 9; each expects symbolic `abcd1234` and gets the same actual source hash. |
| `queue` | 9/9 | 9/9 | 1,500/1,500 | 1,509/1,509 | — |
| `rebase` | 7/7 | 7/7 | 1,500/1,500 | 1,507/1,507 | — |
| `recovery` | 5/5 | 5/5 | 1,500/1,500 | 1,505/1,505 | — |
| `stream` | 10/10 | 10/10 | 1,500/1,500 | 1,510/1,510 | — |
| `session` | 7/7 | 7/7 | 1,500/1,500 | 1,507/1,507 | — |
| `status` | 8/8 | 8/8 | 1,500/1,500 | 1,508/1,508 | — |

## Known conflicts and assertion mismatches

- **Frozen v1.1 CLI help grammar and page bytes:** `cli-01`–`cli-09`, `cli-11`, `cli-12`, `cli-14`, `cli-16`, `cli-17`, `cli-24`, and the help portions of `cli-13`/`cli-21` assert historical help pages or parser behavior. v1.2 fixes the page corpus and says extra help-topic tokens and `--help` are usage errors. `cli-17` expects “no command” where v1.2 says unexpected argument; `cli-24` expects `--help` to display a page where v1.2 requires a usage error. These conflicts are recorded in `docs/work/01-public-cli.md` and the current CLI spec.
- **Status `Next:` line:** `cli-25`, `cli-26`, `state-20`, `state-22`, and `approval-18` freeze status output without the current v1.2 `Next:` line. Recorded in `docs/work/06-status-and-recovery.md` and `docs/work/04-approval-and-refs.md`.
- **Grok account row:** `state-14` and `provider-21` expect only the ChatGPT row, while v1.2 exposes Grok. Recorded in `docs/work/09-chatgpt-accounts.md`.
- **Shape `--json`:** `shape-02`, `shape-14`, `shape-19`, `shape-20`, and `shape-22` expect `intent shape --json`; the v1.2 public CLI has no such option. Recorded in `docs/work/12-shaping-result.md`.
- **Provider wire assertions:** `provider-05`, `provider-13`, `provider-15`, `provider-16`, and `provider-23` have v1.1/v1.2 differences already recorded in `docs/work/10-chatgpt-stream-and-cache.md`. In this run their first assertions are upstream of those differences: queue/build dispatch fails before the provider assertion, so they remain failures rather than counted conflicts/passes.
- **Quint approval hash model:** `approve` and `intent` supply symbolic hashes and caller-provided `prefixOk`; the adapter follows `ADAPTER.md` and computes the approval hash from actual `intent.md` + NUL + acceptance bytes. First byte hash is `eab21f3977c38080f34317c18eaa85177705a2c43a3c20020e5808bce4e42cac`, so approve `sha8` is `eab21f39`; the model expects values such as `aaaa1111`/`abcd1234`. This boundary is documented in `docs/work/04-approval-and-refs.md`; the adapter was not changed to accept symbolic proof.
- **`state-02` regex:** the stdout assertion’s multiline negative lookahead also matches the terminal empty line of normal newline-terminated output, even though the diagnostic continuation lines carry the required prefix. This is an assertion-shape issue, not a CLI output fix.
- **`state-06` lint expectation:** the fixture does not reach the configured style thresholds for `lint_item_too_long` or `lint_sentence_too_long` (A1 has 23 words against a >25 trigger; the Brief has 30 against a >30 trigger). The observed approval card includes the applicable title and long-code warnings, but the case expects the absent item warning.
- **`shape-26` witness:** this v1.1 diagnostic witness expects `Feasibility: PROVEN`; current shape output reports `not checked`. Keep it separate from a guaranteed conformance claim.

## Remaining implementation failures and blocker

The public parser constructs `QueueStart`/`QueueStop`, but `crates/kogen-cli/src/handlers.rs` still falls through to `controller/internal_error: command handler is not available` for those commands. This blocks queue/Build-dependent guaranteed black-box cases across `cli`, `state`, `approval`, `build`, `provider`, `custody`, and v1.2, and causes later “no run found” or fake-request wait timeouts. The `queue`, `rebase`, and `recovery` Quint transition slices pass exactly, but they do not supply the missing public process/Build dispatch. Existing work notes (`docs/work/04-approval-and-refs.md`, `05-serial-queue.md`, `07-gate-and-protection.md`, and `08-cas-landing.md`) record this dependency. Completing the remaining black-box failures requires the queue/Build workflow integration; the pending LIKELY package 13 area was left untouched.

The integration fixes in this worktree are limited to CLI slug boundary validation, config diagnostic indentation, Intent parse line reporting, style warnings in approval cards, workspace-root tool path rendering, and per-checkout shape serialization. Regression coverage was added; `make check` and the previously passing profile cases remain green. No conformance/spec files or package 13 files were edited.
