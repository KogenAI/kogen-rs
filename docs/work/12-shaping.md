# 12 Model shaping and style (L, 5–6 h)

Own shaping submodule under `kogen-core::intent`, style lint data and CLI shape handler. Depends on 02, 03 and 10. Implement exact request bytes, shaper conversations/markers, write scope, staged acceptance check, normalization, reclassification, warnings, requirement ledger and test audit per spec §3.2. Keep proof witness off by default; no hidden public option.

Acceptance: `shape-01`, `shape-03` through `shape-25` inclusive, `format-02`, `format-03`, `format-04`, `format-07`, `format-08`, `format-10`, `state-06`, `state-25`. `shape-02` is a frozen-v1.1 `--json` shape conflict with fixed CLI §1.2; report it, do not add `--json`. `shape-26` witness is E and diagnostic. Quint: none mandatory for shaping; `intent` all 6 hand and 500 × 25 seeds 17, 23, 41 rerun for lifecycle regression.
