# 08 Git CAS landing (G, 5–6 h)

Own landing submodule of `kogen-core::git` and the `kogen-xspec` `rebase` dispatch. Depends on 06 and 07. Implement fresh clone/private index, candidate commit with one expected parent, durable landing record, incoming ref, base compare-and-swap, moved-base re-gate/integration repair, checked-out-base handling and cleanup. No hook or AI trailer. Use spec §2.5.3–2.5.4, §3.9–3.10 and §5.4.

Acceptance: `build-02`, `build-03`, `build-22`, `build-23`, `build-24`, `build-25`, `build-26`, `build-27`, `build-28`, `build-29`, `build-30`, `build-37`, `build-43`, `custody-09`, `custody-10`, `v1.2-06-crash-after-base-cas`. Quint: `rebase` all 7 hand and `recovery` all 5 hand scenarios, each at 500 × 25 for seeds 17, 23, 41. Run effectful CAS traces against a temporary bare origin; assert record-before-CAS, exact parent/tree, re-gate after tip movement, and landed after cleanup crash.
