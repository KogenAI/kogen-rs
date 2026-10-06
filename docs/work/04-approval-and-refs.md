# 04 Approval and immutable refs (G, 5–6 h)

Own `kogen-core::approval`, approval operations in `git`, and `kogen-xspec` `intent`/`approve` dispatch. Depends on 01–03. Implement card/no-hash exit 5, hash-first prefix check, checks, late source re-read, manifest, approval commit/trailers, CAS chain/race, and remove with path-limited commit. Use spec §1.7.2–1.7.3, §2.5, §3.3 and `quint/ADAPTER.md`. A lost CAS retries only as specified; stale bytes preserve the old ref.

Acceptance: `approval-01` through `approval-24` inclusive; `state-08`, `state-09`, `state-10`, `state-25`, `state-28`, `state-29`; `v1.2-02-approval-hash-intent-and-test-bytes`. Quint: `intent` all 6 hand scenarios and `approve` all 18 hand scenarios, then each slice at 500 × 25 with seeds 17, 23, 41. Include late changed Intent/test, same-hash reapproval and two lost-CAS races; observations must derive from real source bytes and temp refs. The adapter never accepts a supplied SHA as proof of `prefixOk`.

## Verification record

Implementation source commit: `1093cf351437948b04160c2976d090caacc352c8`.

Built from that commit with temporary `HOME`:

```sh
task_home=$(mktemp -d) && HOME="$task_home" make check
HOME="$task_home" cargo build -p kogen -p kogen-xspec
```

Both commands passed. `make check` ran formatting, clippy, and the workspace tests (35 core tests passed). Binary SHA-256:

| Binary | SHA-256 |
|---|---|
| `target/debug/kogen` | `1e5d8a7b2c1f4bab55001013ed6c9ef7685f28b18a7cb08219eeaaa22b406b63` |
| `target/debug/kogen-xspec` | `177aab53a66300d5a35780984c669ceaadf1c4cff45f7cba1fb4a0af88c911e5` |

Exact named conformance command:

```sh
case_home=$(mktemp -d) && HOME="$case_home/home" /Users/almirsarajcic/Areas/Kogen/kogen-conformance/bin/kogen-conformance run --kogen target/debug/kogen --profile approval,state,v1.2 --case approval-01,approval-02,approval-03,approval-04,approval-05,approval-06,approval-07,approval-08,approval-09,approval-10,approval-11,approval-12,approval-13,approval-14,approval-15,approval-16,approval-17,approval-18,approval-19,approval-20,approval-21,approval-22,approval-23,approval-24,state-08,state-09,state-10,state-25,state-28,state-29,v1.2-02-approval-hash-intent-and-test-bytes --jobs 4 --workdir "$case_home/work" --out "$case_home/results.jsonl" -v
```

Result: 27/31 cases passed (36 instances; no skipped or unimplemented cases).

- Passed: `approval-01`–`approval-17`, `approval-19`–`approval-21`, `approval-23`, `state-08`, `state-09`, `state-25`, `state-28`, `state-29`, `v1.2-02-approval-hash-intent-and-test-bytes`.
- Failed: `state-10`, `approval-18`, `approval-22`, `approval-24`. These reach commands outside package 04: `state-10` step 4 `queue start` returned `controller/internal_error: command handler is not available` instead of the build/stop/drain lines; `approval-18` step 5 `status` returned exit 70 with that same line instead of `Queue: stopped, 1 waiting; start it with kogen queue start`; `approval-22` step 6 `status` returned exit 70 with that line instead of `Queue: stopped\nNo Intents.\n`; `approval-24` timed out waiting 60 seconds for one fake provider request because queue start did not dispatch. Its remove-blocked assertion passed. Queue/status dispatch is absent from the dependency base in this worktree, so package 04 is not complete until that dependency is merged and these cases pass.

Quint model hand checks passed: `intent` 6/6 and `approve` 18/18. The exact adapter hand comparisons did not fully conform: `intent` 1/6 (only `02-shape-failures` passed); `approve` 3/18 (`04-parse-lint-refuse`, `13-by-invalid`, `14-acceptance-missing` passed). Intent failures: `01-shape-then-approve` step 2 expected `shown=abcd1234`; `03-mismatch-keeps-ref` step 2, `04-reapprove-and-cas` step 2, `05-remove` step 4, and `06-landed-and-missing` step 4 expected approval but observed `intent/hash_mismatch`. Approve failures: `01-card-then-approve`, `02-hash-mismatch`, `03-hash-checked-first`, `05-lint-warnings-on-card`, `06-acceptance-red`, `07-tool-missing`, `08-red-baseline-warns`, `09-baseline-cache`, `10-reapproval`, `11-witness-mode`, `12-identity`, `15-setup-failed`, `16-check-order`, `17-mismatch-keeps-approval`, `18-source-changes-before-cas`.

The first byte-based hand divergence is reproducible from the temp fixture. Exact UTF-8 source bytes (both include the final LF):

```text
---
title: alpha
size: small
domains:
  - platform
---
A concise intent fixture.

## Acceptance
- A1: The source bytes are bound to approval.

## Verify
- A1: test
```

`intent.md` is 164 bytes. The 17-byte acceptance source is `#!/bin/sh\nexit 0\n`. Their SHA-256 over `intent.md`, one NUL, and acceptance is `eab21f3977c38080f34317c18eaa85177705a2c43a3c20020e5808bce4e42cac`. The hand vectors instead supply symbolic `abcd1234`/`aaaa1111` values and a caller-provided `prefixOk`; the adapter computes the hash from those real bytes. For example, hand `intent` scenario `01-shape-then-approve` expects `shown=abcd1234` but receives the full hash above; hand `approve` scenario `01-card-then-approve` expects `sha8=aaaa1111` but receives `eab21f39`. The matching byte-level case `v1.2-02-approval-hash-intent-and-test-bytes` passed. This is the v1.1-style symbolic fixture versus v1.2 raw-byte hash boundary; do not make the adapter accept a supplied SHA to hide it.

Versioned source correction requested for `kogen-spec/quint`: update the `intent` and `approve` hand/generated fixtures to carry actual source bytes and derived hashes, and express real commit/ref observations relationally. Keep the existing symbolic assertions reported as legacy v1.1 expectations and pair the corrected fixtures with v1.2 byte-hash checks. The read-only spec/conformance sources were not changed.

Generated Quint matrices used `gen --traces 500 --steps 25 --seed <seed>` and `conform --only gen` against the committed `kogen-xspec` binary. All six generated matrices passed model invariants for 12,500 events. Exact adapter results (executed steps stop at the first divergence in each trace):

| Slice | Seed | Adapter agreement | Steps observed | First divergence |
|---|---:|---:|---:|---|
| `intent` | 17 | 358/500 | 10,896 | `g0000`, step 13: expected symbolic card hash `abcd1234`; got raw-byte SHA above |
| `intent` | 23 | 357/500 | 10,931 | `g0000`, step 12: expected symbolic card hash `abcd1234`; got raw-byte SHA above |
| `intent` | 41 | 347/500 | 10,958 | `g0000`, step 9: expected symbolic card hash `abcd1234`; got raw-byte SHA above |
| `approve` | 17 | 0/500 | 686 | `g0000`, step 2: expected `environment/setup_failed`; hash-first source check returned `intent/hash_mismatch` (`sha8=3bec7d4e`) |
| `approve` | 23 | 0/500 | 697 | `g0000`, step 1: expected `intent/approval_identity_unavailable`; hash-first source check returned `intent/hash_mismatch` (`sha8=eab21f39`) |
| `approve` | 41 | 0/500 | 699 | `g0000`, step 1: expected `check/acceptance_check_failed`; hash-first source check returned `intent/hash_mismatch` (`sha8=eab21f39`) |

The targeted real-byte adapter control passed on these binaries with temporary `HOME` and isolated temp origins. It supplied `prefixOk=false` while passing the correct source SHA; card exit was 5, the initial approval wrote a real ref, late changed Intent and late changed test each returned `intent/hash_mismatch` while preserving the old ref, same-hash reapproval chained as approval 2, one lost CAS retried in two attempts, and two lost CAS attempts returned `controller/approval_cas_lost` while the observed approval hash/count stayed at the previous value. No provider was contacted by this control.
