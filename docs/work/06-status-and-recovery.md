# 06 Status and recovery (G, 4–6 h)

Own `kogen-core::status`, `recovery`, run snapshot/journal persistence interface and corresponding `kogen-xspec` dispatch. Depends on 05. Derive status from reachable commits, approval refs, claim and runs in the spec's precedence order; render overview/detail/JSON/watch. Reconcile dead owners and post-CAS crash as landed if the landing commit is reachable. Release only the owner claim. Use spec §1.7.5, §2.8–2.11, §3.10.

Acceptance: `cli-25`, `cli-26`, `cli-27`, `cli-28`, `cli-29`, `state-11`, `state-12`, `state-17`, `state-18`, `state-19`, `state-20`, `state-21`, `state-23`, `state-24`, `state-30`, `build-36`, `build-37`, `build-38`, `custody-05`, `v1.2-06-crash-after-base-cas`. Quint: `status` all 8 hand and `recovery` all 5 hand scenarios, each with 500 × 25 at seeds 17, 23, 41. Add the crash-after-base-CAS-before-incoming-cleanup trace. Watch emits a frame only on change.
