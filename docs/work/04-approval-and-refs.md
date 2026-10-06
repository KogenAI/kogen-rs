# 04 Approval and immutable refs (G, 5–6 h)

Own `kogen-core::approval`, approval operations in `git`, and `kogen-xspec` `intent`/`approve` dispatch. Depends on 01–03. Implement card/no-hash exit 5, hash-first prefix check, checks, late source re-read, manifest, approval commit/trailers, CAS chain/race, and remove with path-limited commit. Use spec §1.7.2–1.7.3, §2.5, §3.3 and `quint/ADAPTER.md`. A lost CAS retries only as specified; stale bytes preserve the old ref.

Acceptance: `approval-01` through `approval-24` inclusive; `state-08`, `state-09`, `state-10`, `state-25`, `state-28`, `state-29`; `v1.2-02-approval-hash-intent-and-test-bytes`. Quint: `intent` all 6 hand scenarios and `approve` all 18 hand scenarios, then each slice at 500 × 25 with seeds 17, 23, 41. Include late changed Intent/test, same-hash reapproval and two lost-CAS races; observations must derive from real source bytes and temp refs. The adapter never accepts a supplied SHA as proof of `prefixOk`.
