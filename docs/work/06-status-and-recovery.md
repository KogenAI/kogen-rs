# 06 Status and recovery (G, 4–6 h)

Own `kogen-core::status`, `recovery`, run snapshot/journal persistence interface and corresponding `kogen-xspec` dispatch. Depends on 05. Derive status from reachable commits, approval refs, claim and runs in the spec's precedence order; render overview/detail/JSON/watch. Reconcile dead owners and post-CAS crash as landed if the landing commit is reachable. Release only the owner claim. Use spec §1.7.5, §2.8–2.11, §3.10.

Acceptance: `cli-25`, `cli-26`, `cli-27`, `cli-28`, `cli-29`, `state-11`, `state-12`, `state-17`, `state-18`, `state-19`, `state-20`, `state-21`, `state-23`, `state-24`, `state-30`, `build-36`, `build-37`, `build-38`, `custody-05`, `v1.2-06-crash-after-base-cas`. Quint: `status` all 8 hand and `recovery` all 5 hand scenarios, each with 500 × 25 at seeds 17, 23, 41. Add the crash-after-base-CAS-before-incoming-cleanup trace. Watch emits a frame only on change.

## Verification record (2026-10-07)

Implementation source SHA: `09f47510a6654391aacbc7f7dd3ccea2ca64ef14`.
Conformance source: `kogen-conformance` v1.2 at `c1907685e52b9ba965f540b6cc162b489d4f4334` (read-only).
Binary SHA-256: `target/debug/kogen` = `b05d7416b8d8c44df082c4e2f102ac77ffbfcac3d5140ea2afd5a74af37383bd`; `target/debug/kogen-xspec` = `f1bbc2ec42b8fa455da4d6d696ae3e708fbc211f4f96140fa7ac448c47215b89`.

`make check` passed: formatting, Clippy with warnings denied, 75 core tests, and 2 xspec tests. The explicit crash trace also passed:

```sh
cargo test -p kogen-core crash_after_base_cas_before_incoming_cleanup_reconciles_as_landed
```

The trace creates a base branch that reaches the landing candidate while the incoming ref and owner claim still exist, then verifies recovery records `landed`/`reconciled`, appends the reconciliation event, and deletes only the matching claim and incoming ref.

The exact named-case command was:

It ran from `/Users/almirsarajcic/Areas/Kogen/kogen-conformance`; the runner supplied isolated per-case `HOME`/`TMPDIR` and fake provider endpoints.

```sh
/Users/almirsarajcic/Areas/Kogen/kogen-conformance/bin/kogen-conformance run --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/06-status-and-recovery/target/debug/kogen --case cli-25,cli-26,cli-27,cli-28,cli-29,state-11,state-12,state-17,state-18,state-19,state-20,state-21,state-23,state-24,state-30,build-36,build-37,build-38,custody-05,v1.2-06-crash-after-base-cas --jobs 4 --workdir /tmp/kogen-status-final-cases.dpPdCH/work --out /tmp/kogen-status-final-cases.dpPdCH/results.jsonl --keep --quiet
```

Pass IDs: `cli-28`, `state-17`, `state-19`, `state-21`, `state-24`, `state-30`, `build-37` (7/20). Fail IDs: `cli-25`, `cli-26`, `cli-27`, `cli-29`, `state-11`, `state-12`, `state-18`, `state-20`, `state-23`, `build-36`, `build-38`, `custody-05`, `v1.2-06-crash-after-base-cas` (13/20; none skipped or unimplemented).

Three failures are v1.1/v1.2 expectation conflicts:

- `cli-25` expected 21 lines and `Queued:` immediately after the queue line. The observed v1.2 bytes include `Next: greet (priority 0; no dependencies; ties by approval time and slug)\n` before `Queued:`, for 22 lines.
- `cli-26` step 16 expected `  verdict: unverified`; observed output has `  candidate diff: /private/tmp/kogen-status-final-cases.dpPdCH/work/cli-26/home/.kogen/workspaces/checkout-3183715dae/runs/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/candidate.diff` at that line and no `verdict:` line, as current v1.2 §1.7.5 specifies.
- `state-20` expected 3 lines; observed bytes are `Queue: stopped, 1 waiting; start it with kogen queue start\nNext: greet (priority 0; no dependencies; ties by approval time and slug)\nQueued:\n  greet\n` (4 lines).

Captured stdout bytes:

```text
cli-25:
Queue: stopped, 3 waiting; start it with kogen queue start
Next: greet (priority 0; no dependencies; ties by approval time and slug)
Queued:
  greet
  queue-b
  queue-cc
Failed:
  fail-a  repair_cap (Build a1a1a1a1)
Parked:
  park-bb  landing_retries (Build b2b2b2b2)
Interrupted:
  intr-a  interrupted (Build c3c3c3c3)
Drafts:
  draft-a
  draft-bbb
Landed (7):
  land-g  4033b434
  land-f  2351ffcb
  land-e  19cbd8b6
  land-d  ab06ddca
  land-c  3a4d827e
  and 2 earlier

cli-26:
fail-a: failed, repair_cap
Build a1a1a1a1: failed, repair_cap
  candidate diff: /private/tmp/kogen-status-final-cases.dpPdCH/work/cli-26/home/.kogen/workspaces/checkout-3183715dae/runs/a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1/candidate.diff
  journal: /private/tmp/kogen-status-final-cases.dpPdCH/work/cli-26/home/.kogen/workspaces/checkout-3183715dae/runs/a1a1a1a1a1a1a1a1a1a1a1a1a1a1

state-20:
Queue: stopped, 1 waiting; start it with kogen queue start
Next: greet (priority 0; no dependencies; ties by approval time and slug)
Queued:
  greet
```

The other ten failures enter the unavailable queue/Build workflow. `state-11` and `state-12` observe `controller/internal_error: command handler is not available`; the queue-start cases otherwise time out waiting for a fake provider request or, for `v1.2-06-crash-after-base-cas`, the `{case}/post-cas` marker. These scenarios need the downstream `08-cas-landing` workflow in addition to this package. The direct post-CAS recovery trace above passes.

Requested versioned source correction: update/version the frozen v1.1 status expectations for `cli-25`, `cli-26`, and `state-20` to match the v1.2 `Next:` and no-`verdict:` contract. The supplied conformance checkout is read-only, so no case or expectation was changed.

Quint replay used complete observations, with no `--project` projection. Hand results: status 8/8; recovery 5/5. Generated results: each slice passed 500 traces × 25 steps at seeds 17, 23, and 41 (500/500 traces and 12,500/12,500 transitions per run). First divergence: none. Commands used `xspec.py gen --traces 500 --steps 25 --seed {17,23,41}` followed by `xspec.py conform --only gen -- .../target/debug/kogen-xspec status|recovery`; hand replay used `xspec.py conform --only hand -- .../target/debug/kogen-xspec status|recovery`.
