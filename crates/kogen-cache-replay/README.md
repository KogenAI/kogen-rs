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
kogen-cache-replay plan --fixtures /tmp/claude-501/krs-cache-replay/fixtures.json --seed cache-study-20261007 --endpoints openai --out /tmp/claude-501/krs-cache-replay/openai/plan.json
kogen-cache-replay dry-run --plan /tmp/claude-501/krs-cache-replay/openai/plan.json
kogen-cache-replay execute --plan /tmp/claude-501/krs-cache-replay/openai/plan.json --max-posts 180 --max-total-tokens 1000000 --out /tmp/claude-501/krs-cache-replay/openai/receipts

kogen-cache-replay plan --fixtures /tmp/claude-501/krs-cache-replay/fixtures.json --seed cache-study-20261007 --endpoints chatgpt --out /tmp/claude-501/krs-cache-replay/chatgpt/plan.json
kogen-cache-replay dry-run --plan /tmp/claude-501/krs-cache-replay/chatgpt/plan.json
kogen-cache-replay execute --plan /tmp/claude-501/krs-cache-replay/chatgpt/plan.json --max-posts 180 --max-total-tokens 1000000 --out /tmp/claude-501/krs-cache-replay/chatgpt/receipts

# Both endpoints, when the full 2M-token budget is intended:
kogen-cache-replay plan --fixtures /tmp/claude-501/krs-cache-replay/fixtures.json --seed cache-study-20261007 --endpoints both --out /tmp/claude-501/krs-cache-replay/both/plan.json
```

The frozen plan records request bodies and hashes, randomized order, endpoint
output-limit fields, header masks, and per-request input/output reservations.
`dry-run` prints each request's body size, prefix length, header factors, and
reservation without dispatching it. `--endpoints` defaults to `both`. Each
single-endpoint plan keeps that endpoint's 180 adjacent primer/probe requests,
including its complete AB/BA, cold/warm, header, and scope schedule. Its hard
caps are 180 POSTs and 1,000,000 tokens; the frozen worst-case reservation is
970,752 tokens for each split. The two split plan hashes are deterministic and
distinct.

The first live run stopped on its first backend request because Kogen's
`OwnedBackend` adapter requires an account ID. For an Owned login Kogen derives
that value from the `chatgpt_account_id` ID-token claim; the selected login
returned no account ID, so wire construction failed before body validation or
dispatch. The injected credential path supplies its account ID directly.

`KOGEN_PROVIDER_URL` is rejected. Execution uses `KOGEN_AUTH_PATH` from the
host's established injected-auth environment when it is set; `--auth-path` is
also supported. Without either, execution uses Kogen's normal selected Owned
ChatGPT account and refresh path. Kogen re-reads injected credentials for each
request. Keep the injected credential on the benchmark host; never put it in
the source bundle, fixtures, plan, or receipts.

## Linux benchmark host

Build from the unpacked source bundle on the benchmark host. Include the
sanitized `fixtures.json` alongside the bundle, but do not include credentials.
The plan pins the binary SHA-256, so generate the execution plan with the Linux
binary after building it. This also ensures the plan hash reflects the host
build. The R74 host must set `KOGEN_AUTH_PATH` through its established
injection before running these commands:

```sh
cd /path/to/unpacked/kogen-cache-replay-source
CARGO_TARGET_DIR=/tmp/krs-target cargo build --release --locked -p kogen-cache-replay
REPLAY_BIN=/tmp/krs-target/release/kogen-cache-replay
REPLAY_DIR=/tmp/krs-cache-replay
: "${KOGEN_AUTH_PATH:?R74 host injection must set KOGEN_AUTH_PATH}"
mkdir -p "$REPLAY_DIR/chatgpt"
"$REPLAY_BIN" plan --fixtures "$REPLAY_DIR/fixtures.json" --seed cache-study-20261007 --endpoints chatgpt --out "$REPLAY_DIR/chatgpt/plan.json"
"$REPLAY_BIN" dry-run --plan "$REPLAY_DIR/chatgpt/plan.json" > "$REPLAY_DIR/chatgpt/dry-run.json"
"$REPLAY_BIN" execute --plan "$REPLAY_DIR/chatgpt/plan.json" --max-posts 180 --max-total-tokens 1000000 --out "$REPLAY_DIR/chatgpt/receipts"
```

The OpenAI split command is the same with `--endpoints openai`, an `openai`
plan/receipt directory, and no injected-auth requirement. The `both` plan uses
the full 360 POST and 2,000,000 token cap.
