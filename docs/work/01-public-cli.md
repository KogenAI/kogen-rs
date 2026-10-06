# 01 Public CLI and help (G, 3–5 h)

Own `crates/kogen-cli/`, embedded `data/help/`, and CLI renderer types in `kogen-core::error`. Depends on no package. Copy the 16 current pages byte-for-byte from `../../../kogen-spec/spec/data/help/`; implement only the grammar in CLI-RULE and spec §1.1–1.5. Both `chatgpt` and `grok` are provider *names* on existing verbs. Wire command handlers to typed core interfaces as later packages arrive; no extra public verbs or flags.

Acceptance: run `cli-01` through `cli-24` (every ID in that inclusive range), `format-01`, `format-06`, `format-09`, `v1.2-01-fixed-cli-help-and-grok`. Pass every compatible row. `cli-02`, `cli-11` and `format-01` contain frozen provider help/name expectations and are reported as v1.1/v1.2 conflicts when they disagree; the v1.2 case is the required exact help oracle. Account and project actions in these cases use typed handler results as necessary. Quint: none; there is no CLI grammar slice.
