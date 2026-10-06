# 14 Setup cache (L, 3–5 h)

Own `kogen-core::run::setup_cache` and approval baseline cache; depends on 03 and 07. Implement canonical key over base tree, configured setup/env/outputs and inputs; atomic publish, copy-on-write restore, no failed entry, three-entry LRU, and baseline reuse. Use spec §2.9. Document any cross-language change to runtime-specific `elixir`/`otp` key fields before promoting the full contract.

Acceptance: `state-26`, `state-27`, `state-28`, `approval-12`, `approval-13`, `build-39`. Quint: `setup-cache` all 7 hand scenarios and 500 × 25 seeds 17, 23, 41 are diagnostic while the exact key remains L. A cache hit must not share writable output files with another workspace.
