# 14 Setup cache (L, 3–5 h)

Own `kogen-core::run::setup_cache` and approval baseline cache; depends on 03 and 07. Implement canonical key over base tree, configured setup/env/outputs and inputs; atomic publish, copy-on-write restore, no failed entry, three-entry LRU, and baseline reuse. Use spec §2.9. Document any cross-language change to runtime-specific `elixir`/`otp` key fields before promoting the full contract.

Acceptance: `state-26`, `state-27`, `state-28`, `approval-12`, `approval-13`, `build-39`. Quint: `setup-cache` all 7 hand scenarios and 500 × 25 seeds 17, 23, 41 are diagnostic while the exact key remains L. A cache hit must not share writable output files with another workspace.

## Implementation notes

The Rust cache hashes compact JSON with recursively sorted object keys. Declared setup inputs replace `base_tree`; input records are sorted by path and use Git-compatible numeric modes (`33188` for `100644`, `33261` for `100755`, and `40960` for `120000`) plus SHA-256 of their bytes. Missing declared inputs use mode `0` and SHA-256 of `kogen:absent`.

The v1.2 `elixir` and `otp` fields remain in the key. Rust reads `ELIXIR_VERSION` (then `MISE_ELIXIR_VERSION`) and `OTP_VERSION` (then `ERLANG_VERSION` or `MISE_ERLANG_VERSION`) from the constructed child environment; absent versions are empty strings. No cross-language key-field change is proposed. A cross-language canonical digest fixture is still required before promoting this L slice.

Setup products are copied into independent workspace files, never hard-linked. Only successful setups whose non-output workspace fingerprint stays stable publish an entry. Publication renames a complete temporary directory; baseline JSON uses a synced temporary file and rename. A baseline-cache hit returns before setup-output restoration, preserving check side effects from its original run.

## Verification record

- Implementation source: `c788992e47b6d6ee0d6705c9781a16d1fd32d19e`.
- `kogen` binary SHA-256: `8e5b8c790ca1a05667d2b0501e842facffd1560ea88196af71e6ecdd2824dd4f`.
- `kogen-xspec` binary SHA-256: `de5ed4256883e9c5913b565c615471068ddfdc84cd988df39e5107e1b1d5a4c6`.
- Spec checkout SHA: `1118f7fc0dbb042af8c8de2ffd1b85768cf9e2a0`. Read-only conformance checkout SHA: `c1907685e52b9ba965f540b6cc162b489d4f4334`.
- `make check`: pass. It ran rustfmt, clippy with warnings denied, and the full workspace tests (90 core tests and 2 xspec tests).
- Setup-cache unit tests: 4/4 pass, including failed publication, LRU, canonical key context, and independent writable restore.
- Quint hand scenarios: 7/7 pass through the spec and 7/7 exact replay through `kogen-xspec`.
- Quint generated traces: seeds 17, 23, and 41; 500 traces × 25 steps per seed; 500/500 exact full-observation replays per seed. First divergence: none.

Exact named-case command (run from `/Users/almirsarajcic/Areas/Kogen/kogen-conformance`):

```sh
env HOME="$(mktemp -d)" KOGEN_PROVIDER_URL=http://127.0.0.1:9 python3 -m kogen_conformance run --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/14-setup-cache/target/debug/kogen --case state-26,state-27,state-28,approval-12,approval-13,build-39 --workdir /tmp/kogen-14-setup-cases.7DxORg/sha-work --out /tmp/kogen-14-setup-cases.7DxORg/sha-results.jsonl --time-scale 0.01 -v
```

Pass IDs: `state-28`, `approval-12`, `approval-13`. Fail IDs: `state-26`, `state-27`, `build-39`. No cases were skipped or unimplemented. `state-26` step 4 and `state-27` step 13 expected `kogen queue start` to run, but it returned exit 70 with `controller/internal_error: command handler is not available`. `build-39` step 4 hit the same unavailable handler, so its run assertion reported `no run found for greet`. These Build assertions need the separate queue/Build command handler; they are not setup-cache passes or a spec conflict. The package remains L and is not promoted.
