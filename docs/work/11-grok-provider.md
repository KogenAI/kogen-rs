# 11 Grok on the existing provider verbs (G name, L wire, 4–6 h)

Own `kogen-core::provider::grok` and Grok account rows. Depends on 09 and 10. The CLI name is already admitted in package 01; implement Grok `login`, `logout`, `use`, device-code OAuth/refresh, selected account/model, request headers/body and error mapping per spec §4.10. Share SSE, cache history, retries and credential abstraction with ChatGPT. Never add a public flag or command.

Acceptance: `v1.2-01-fixed-cli-help-and-grok` and the proposed `P10` observations in `../../../kogen-spec/spec/CONFORMANCE-v1.2-CASES.md` (no named suite file yet for full Grok wire). Rerun the retry and cache scenarios using Grok fake HTTP as local integration checks; `provider-11` through `provider-18` and `v1.2-03-consecutive-request-byte-prefix` remain ChatGPT black-box IDs. Quint: `stream` all 10 and `session` all 7 hand scenarios at 500 × 25 seeds 17, 23, 41 using shared production policy; `accounts` 9 hand plus generated seeds are diagnostic. Report the suite coverage gap explicitly until P10 is versioned into a black-box case.

## Verification record

- Source SHA: `d69247bf14282cf133e3381615ed71051c9f6831` (`Add Grok provider support`).
- Binary SHA-256 after `cargo build -p kogen -p kogen-xspec`:
  - `target/debug/kogen`: `202d402f1cafd4e9647e2064e5397d030ebe2f215fe4206c636a5964bb53947a`
  - `target/debug/kogen-xspec`: `c72afd8d4d27252973c612f8e527b94acedc0913743e023a8b00dbb70ee45cb7`
- `make check`: pass (fmt, Clippy `-D warnings`, 64 workspace tests).
- `cargo test -p kogen-core grok_ -- --nocapture`: 5 passed, including local fake HTTP refresh, retry/error mapping, and retry/cache-prefix request checks.
- Device OAuth/account CLI integration used a temporary `HOME` and loopback fake HTTP endpoints. Discovery, device form, wait-before-first-poll, pending response, token persistence and mode 0600, `use` rows, `list`, and local `logout` passed.

Frozen black-box case command and result:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen "$PWD/target/debug/kogen" --profile v1.2 --case v1.2-01-fixed-cli-help-and-grok --jobs 1 --out /tmp/kogen-grok-v1.2-case-final.jsonl
```

Result: 21/21 instances passed. Pass ID: `v1.2-01-fixed-cli-help-and-grok`. Fail IDs: none. The proposed P10 observations have no versioned black-box case, so Grok request wire coverage remains a suite gap. `provider-11` through `provider-18` and `v1.2-03-consecutive-request-byte-prefix` were not counted for Grok; they remain ChatGPT cases.

Quint checks used temporary copies of the read-only slice directories and temporary goldens. Hand scenario agreement: `stream` 10/10; `session` 7/7. For each seed 17, 23, and 41, generation used `--traces 500 --steps 25`; full observation conformance through `target/debug/kogen-xspec` was:

| Slice | Seed 17 | Seed 23 | Seed 41 | First divergence |
|---|---:|---:|---:|---|
| `stream` | 510/510; 12,578 steps | 510/510; 12,578 steps | 510/510; 12,578 steps | none |
| `session` | 507/507; 12,532 steps | 507/507; 12,532 steps | 507/507; 12,532 steps | none |

Diagnostic `accounts`: spec hand scenarios 9/9; generated invariant checks passed for 500 × 25 at each seed 17, 23, and 41. Adapter conformance was 0/509 traces and 0 steps for each seed. First divergence: `reset`; exact adapter error: `adapter gave no answer to {"op": "reset"} kogen-xspec: unsupported slice "accounts"`. This is an unimplemented diagnostic adapter slice, not a pass or a product behavior claim.
