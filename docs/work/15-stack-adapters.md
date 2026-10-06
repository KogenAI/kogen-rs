# 15 Rails and ExUnit adapters (L, 4–6 h)

Own `kogen-core::gate::adapters::{rails,exunit}` only; depends on 03 and 07. Build on the common command adapter and ledger. Implement Rails two-file detection, source/candidate paths, runner, formatter and gate files; implement ExUnit ledger formatter, setup seeds, unavailable detection and finding parsers per spec §2.4.3–2.4.4. No changes to queue, approval or provider policy.

Acceptance: `exunit-01` through `exunit-06` inclusive when Elixir is installed; `state-03`, `build-14`, `build-19`; proposed `P12` Rails detection in `../../../kogen-spec/spec/CONFORMANCE-v1.2-CASES.md` (no named suite file yet). Quint: full `gate` all 5 hand and 500 × 25 seeds 17, 23, 41 is diagnostic for adapter regression. Missing Elixir is reported as unavailable, never counted as a pass.
