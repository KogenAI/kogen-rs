# 05 Serial queue and origin claim (G, 4–6 h)

Own `kogen-core::queue` and its `kogen-xspec` dispatch. Depends on 04. Implement one claim per origin, priority descending then approval time then slug, duplicate start, stop after current, detach, branch skip, single processing per drain, failure-class continuation and live/stale ownership. Use spec §1.7.4, §2.11 and §3.11. Keep the scheduler independent of status rendering.

Acceptance: `build-01`, `build-31`, `build-32`, `build-33`, `build-34`, `build-35`, `build-41`, `build-42`, `cli-30`, `state-20`, `state-22`. Quint: `queue` all 9 hand scenarios, including priority and ordinary provider failure, plus 500 × 25 at seeds 17, 23, 41. Add same-hash reapproval and live second-start traces. Exact line, phase and exit observations must come from the production scheduler.
