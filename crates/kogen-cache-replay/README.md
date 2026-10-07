# kogen-cache-replay

Private development binary for offline prompt-cache replay planning and a
single-attempt runner. `plan` and `dry-run` do not load credentials or make
network requests. The runner uses Kogen's account selection, auth refresh,
wire builder, and one-attempt `ReqwestPort` path.

## Admission and budget policy

Plans are admitted under `coordinator_cancel_on_overflow_v1`. A hard per-request
output cap is not an admission prerequisite. The runner sends
`max_output_tokens=256` on both allowlisted endpoints, then cancels a response
stream when its output exceeds 512 local/provider-reported tokens or its
reasoning count exceeds 1,024. Such attempts are marked `truncated` in the
receipt ledger. The SSE transport monitors deltas and usage events while
streaming; local text counts use the frozen `whitespace-v1` tokenizer.

`--max-posts` and `--max-total-tokens` remain hard stops. Before each POST the
runner reserves that request's input allowance, the endpoint's requested
output limit, and the observed framing margin. Provider-reported input and
output totals are charged as soon as the response ends; if usage is partial or
unknown, the complete request reservation is retained. The runner stops before
dispatch when the next reservation would exceed the remaining token budget,
and the stream guard also cancels when the remaining global output allowance
is reached.

## Sanitized fixtures

The fixture manifest is built from the static builder instruction in
`crates/kogen-core/src/build/provider_prompt.rs`, the shaper system instruction
in `crates/kogen-core/src/intent/shaping/prompts.rs`, and role-filtered schemas
from `kogen_core::provider::tools::canonical_tool_schemas()`. The task text is
synthetic; no journal prompt text is used. Each fixture is marked
`reconstructed`, records its source prompt builder and sanitized-material hash,
and includes the frozen continuation. Neutral `pad` tokens extend the complete
static prefix to 512, 2,048, or 11,008 `whitespace-v1` tokens. The schema
reference is text in the common prompt prefix; no native tools are sent or
executed.

The manifest is `/tmp/claude-501/krs-cache-replay/fixtures.json` for this
replay. Keep plans and receipts outside the repository.

## Commands

```text
kogen-cache-replay plan --fixtures /tmp/claude-501/krs-cache-replay/fixtures.json --seed cache-study-20261007 --out /tmp/claude-501/krs-cache-replay/plan.json
kogen-cache-replay dry-run --plan /tmp/claude-501/krs-cache-replay/plan.json
kogen-cache-replay execute --plan /tmp/claude-501/krs-cache-replay/plan.json --max-posts 360 --max-total-tokens 2000000 --out /tmp/claude-501/krs-cache-replay/receipts
```

The frozen plan records request bodies and hashes, randomized order, endpoint
output-limit fields, header masks, and per-request input/output reservations.
`dry-run` prints each request's body size, prefix length, header factors, and
reservation without dispatching it.

`KOGEN_PROVIDER_URL` is rejected. Ambient `KOGEN_AUTH_PATH` is rejected unless
`--auth-path` explicitly selects Kogen injected credentials. Without that
option, execution uses Kogen's normal selected Owned ChatGPT account and
refresh path. Receipts retain raw usage and HTTP status but omit auth headers,
response text, provider response IDs, and turn-state values.
