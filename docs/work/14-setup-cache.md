# 14 Setup cache (L, 3–5 h)

Own `kogen-core::run::setup_cache` and approval baseline cache; depends on 03 and 07. Implement canonical key over base tree, configured setup/env/outputs and inputs; atomic publish, copy-on-write restore, no failed entry, three-entry LRU, and baseline reuse. Use spec §2.9. Document any cross-language change to runtime-specific `elixir`/`otp` key fields before promoting the full contract.

Acceptance: `state-26`, `state-27`, `state-28`, `approval-12`, `approval-13`, `build-39`. Quint: `setup-cache` all 7 hand scenarios and 500 × 25 seeds 17, 23, 41 are diagnostic while the exact key remains L. A cache hit must not share writable output files with another workspace.

## Implementation notes

The Rust cache hashes compact JSON with recursively sorted object keys. Declared setup inputs replace `base_tree`; input records are sorted by path and use Git-compatible numeric modes (`33188` for `100644`, `33261` for `100755`, and `40960` for `120000`) plus SHA-256 of their bytes. Missing declared inputs use mode `0` and SHA-256 of `kogen:absent`.

The v1.2 `elixir` and `otp` fields remain in the key. Rust reads `ELIXIR_VERSION` (then `MISE_ELIXIR_VERSION`) and `OTP_VERSION` (then `ERLANG_VERSION` or `MISE_ERLANG_VERSION`) from the constructed child environment; absent versions are empty strings. No cross-language key-field change is proposed. A cross-language canonical digest fixture is still required before promoting this L slice.

Setup products are copied into independent workspace files, never hard-linked. Only successful setups whose non-output workspace fingerprint stays stable publish an entry. Publication renames a complete temporary directory; baseline JSON uses a synced temporary file and rename. A baseline-cache hit returns before setup-output restoration, preserving check side effects from its original run.
