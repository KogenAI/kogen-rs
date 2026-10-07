# kogen-cache-replay

Private development binary for the controlled prompt-cache replay design. It
does not add a `kogen` command. `plan` and `dry-run` are offline. `execute`
uses Kogen's account selection, auth refresh, wire builder, and one-attempt
`ReqwestPort` path.

## Sanitized fixture manifest

The tool accepts two already-sanitized Kogen-derived excerpts, with IDs
`builder` and `shaper`. It does not read Kogen journals, sanitize raw request
bodies, inspect credential files, or reconstruct missing source bodies. Use
the retained offline source to produce a manifest like this before planning:

```json
{
  "schema_version": 1,
  "fixtures": [
    {
      "id": "builder",
      "source_run": "owned-run-identifier",
      "source_request_ordinal": 1,
      "source_adapter": "owned",
      "source_body_sha256": "<64 hex characters>",
      "provenance": "captured",
      "sanitized": true,
      "source_model": "gpt-6-luna",
      "source_effort": "medium",
      "instructions": "Sanitized source instructions",
      "input_excerpt": "Sanitized source input excerpt"
    },
    {
      "id": "shaper",
      "source_run": "shape-run-identifier",
      "source_request_ordinal": 2,
      "source_adapter": "shaper",
      "source_body_sha256": "<64 hex characters>",
      "provenance": "reconstructed",
      "sanitized": true,
      "source_model": "gpt-6-luna",
      "source_effort": "medium",
      "instructions": "Sanitized source instructions",
      "input_excerpt": "Sanitized source input excerpt"
    }
  ]
}
```

`source_body_sha256` is the digest of the retained original Kogen request body;
the excerpts are the separately sanitized material used by the replay. The
planner counts whitespace-delimited local tokens (`whitespace-v1`), truncates
fixture instructions to each target prefix, and limits the excerpt to 384
local tokens. It does not claim those counts equal provider tokenization.
Warm probes preserve the primer's input item byte-for-byte and append the fixed
continuation as a new input item; cold sham primers and their fresh probes use
the same two input items with independently salted static prefixes.

## Commands

```text
kogen-cache-replay plan --fixtures sanitized-fixtures.json --seed cache-study-20261007 --out /tmp/cache-replay/plan.json
kogen-cache-replay dry-run --plan /tmp/cache-replay/plan.json
kogen-cache-replay execute --plan /tmp/cache-replay/plan.json --max-posts 360 --max-total-tokens 2000000 --out /tmp/cache-replay/receipts
```

The plan freezes 360 request bodies, SHA-256 hashes, randomized order, IDs,
header masks, and the 256-token `max_output_tokens` field. The ChatGPT backend
endpoint has no versioned generation-cap contract in this checkout. Therefore
the full two-endpoint plan is marked `not admitted`. `execute` records every
scheduled slot as unexecuted in a zero-dispatch receipt ledger before
exiting with the blocker; it does not load login state or send a POST. Do not
clear that blocker without a versioned endpoint contract and a bounded
admission result.

The optional `--auth-path` is explicit Kogen injected auth via
`kogen_core::provider::auth::read_injected`. Ambient `KOGEN_AUTH_PATH` and
`KOGEN_PROVIDER_URL` are rejected. Without `--auth-path`, every attempt uses
Kogen's normal selected Owned ChatGPT account and refresh path. Outputs must
be outside the repository. `attempts.jsonl` is flushed after every scheduled
slot; raw usage and HTTP status are retained, while auth headers, response
text, provider response IDs, and turn-state values are not written.
