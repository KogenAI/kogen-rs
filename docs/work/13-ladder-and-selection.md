# 13 Ladder, repairs and candidate selection (L, 5–6 h)

Own `kogen-core::run::orchestration` and selector/auditor submodules. Depends on 07, 08 and 10. Implement started-Build terminal semantics, repair/fresh attempt/rung progression, best-candidate preservation, model `finish`, report fields and selected L recipes per spec §3.1, §3.4–3.8. Keep guaranteed green gate and CAS independent. Record where the frozen v1.1 rung policy disagrees with v1.2.

Acceptance: `build-04` through `build-08` inclusive, `build-31`, `build-39`, `build-44`, `ladder-01` through `ladder-36` inclusive, with `ladder-03`, `ladder-26`, `ladder-31` explicitly reported as version conflicts where their old policy disagrees. Quint: `orchestration` all 6 hand and `gate` all 5 hand scenarios plus 500 × 25 seeds 17, 23, 41 are diagnostic until L policies are promoted. Rerun the guaranteed `queue`, `rebase`, and `recovery` slices for regression; the green gate remains a black-box obligation.
