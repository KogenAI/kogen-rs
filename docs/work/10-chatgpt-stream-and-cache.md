# 10 ChatGPT streaming and cache affinity (G, 5–6 h)

Own `kogen-core::provider::{http,sse,session,tools}` and its `kogen-xspec` `stream`/`session` dispatch. Depends on 03 and 09. Implement owned/injected Responses wire, deterministic input-last JSON, append-only history, stable Build cache key, distinct conversation thread, SSE assembly, partial-stream continuation, bounded retries/waits/fallback, tool dispatch and usage null handling. Use spec §4.1–4.9 and the source paths in `ARCHITECTURE.md`. The private adapter injects fake clock/HTTP results into production policy.

Acceptance: run `provider-01` through `provider-20` inclusive, `provider-23` through `provider-26` inclusive, `v1.2-03-consecutive-request-byte-prefix`, `v1.2-04-cache-key-session-headers`, `v1.2-05-missing-usage`, `custody-08`. `provider-05` encodes the old cache-key formula; report its v1.1/v1.2 conflict and use `v1.2-04` for the Build-affinity rule. Other old deadline cases are compared against §4.5 before claiming compatibility. `provider-21`/`22` belong to package 09. Quint: `stream` all 10 and `session` all 7 hand scenarios, each at 500 × 25 seeds 17, 23, 41. Add identical retry, third turn and model switch traces. Fake-provider requests must show byte-prefix/header behavior and missing usage as null. Separately run the credential-safe live three-turn >95% warm-cache smoke before release, reporting every warm numerator/denominator and missing usage; a fake pass alone does not satisfy that gate.

## Implementation and verification record

The provider modules now own ordered Responses request construction, streamed SSE parsing and response assembly, retry/fallback/wait decisions, Build cache affinity, distinct conversation ids, append-only continuation history, tool dispatch, result budgets, and nullable usage. The private `stream` and `session` replayers use the production transition structs. Fake HTTP and clock tests inspect exact request bytes and headers across identical retries, three turns, partial continuation, and model fallback.

Source bundle SHA-256 (manifest: provider `http`/`session`/`sse`/`tools`, `provider/mod.rs`, xspec dispatcher, and their Cargo manifests/lockfile; this note excluded): `9f91f7c152ce6559883b07648ce9c6033f8b1d9297988b952e31e0f207f0441d`.

Binary SHA-256:

- `target/debug/kogen`: `13c6957b0546fe263170f974c49214900988036b80041a9b663b0d0530806eec`
- `target/debug/kogen-xspec`: `aabf26cbf55786a74539edeed3ee18484d8f6a8f9c4c3f8eac5a346388ece401`

`make check` passed: formatting, workspace Clippy with warnings denied, and 59 workspace tests.

### Quint

The copied slices were run from `/tmp/kogen-10-source-traces.DSHtQt`; the read-only spec checkout was not modified.

| Slice | Hand | Seed 17 | Seed 23 | Seed 41 | First divergence |
|---|---:|---:|---:|---:|---|
| `stream` | 10/10 | 510/510, 12,578 steps | 510/510, 12,578 steps | 510/510, 12,578 steps | none |
| `session` | 7/7 | 507/507, 12,532 steps | 507/507, 12,532 steps | 507/507, 12,532 steps | none |

Each generated run used `--traces 500 --steps 25`; each `conform` included every hand scenario and all 500 generated traces.

```sh
TRACE_ROOT=/tmp/kogen-10-source-traces.DSHtQt
CONFORMANCE_ROOT=/Users/almirsarajcic/Areas/Kogen/kogen-spec/quint
cp -R "$CONFORMANCE_ROOT/slices/stream" "$TRACE_ROOT/stream"
cp -R "$CONFORMANCE_ROOT/slices/session" "$TRACE_ROOT/session"
XSPEC_SLICE="$TRACE_ROOT/stream" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" spec
for SEED in 17 23 41; do
  XSPEC_SLICE="$TRACE_ROOT/stream" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" gen --traces 500 --steps 25 --seed "$SEED"
  XSPEC_SLICE="$TRACE_ROOT/stream" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" conform -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/10-chatgpt-stream-and-cache/target/debug/kogen-xspec stream
done
XSPEC_SLICE="$TRACE_ROOT/session" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" spec
for SEED in 17 23 41; do
  XSPEC_SLICE="$TRACE_ROOT/session" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" gen --traces 500 --steps 25 --seed "$SEED"
  XSPEC_SLICE="$TRACE_ROOT/session" python3 "$CONFORMANCE_ROOT/prototype/harness/xspec.py" conform -- /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/10-chatgpt-stream-and-cache/target/debug/kogen-xspec session
done
```

### Black-box cases

Exact command (temporary `HOME`, no skip/projection flags):

```sh
HOME=/tmp/kogen-10-source-home.eeK6Gk /Users/almirsarajcic/Areas/Kogen/kogen-conformance/bin/kogen-conformance run --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/10-chatgpt-stream-and-cache/target/debug/kogen --profile provider,v1.2,custody --case provider-01,provider-02,provider-03,provider-04,provider-05,provider-06,provider-07,provider-08,provider-09,provider-10,provider-11,provider-12,provider-13,provider-14,provider-15,provider-16,provider-17,provider-18,provider-19,provider-20,provider-23,provider-24,provider-25,provider-26,v1.2-03-consecutive-request-byte-prefix,v1.2-04-cache-key-session-headers,v1.2-05-missing-usage,custody-08 --jobs 1 --workdir /tmp/kogen-10-source-conformance.CKS8YQ --out /tmp/kogen-10-source-conformance.CKS8YQ/results.jsonl --verbose
```

Result: 0 passed, 28 failed, 0 errors, 0 skipped, 0 unimplemented (38 total instances). Pass IDs: none. Fail IDs: `provider-01`, `provider-02`, `provider-03`, `provider-04`, `provider-05`, `provider-06`, `provider-07`, `provider-08`, `provider-09`, `provider-10`, `provider-11`, `provider-12`, `provider-13`, `provider-14`, `provider-15`, `provider-16`, `provider-17`, `provider-18`, `provider-19`, `provider-20`, `provider-23`, `provider-24`, `provider-25`, `provider-26`, `v1.2-03-consecutive-request-byte-prefix`, `v1.2-04-cache-key-session-headers`, `v1.2-05-missing-usage`, and `custody-08`. Every case stopped before provider traffic: the public `kogen` binary returned `controller/internal_error: command handler is not available` at approval or shaping, and the fake server observed no requests. For example, `provider-05` failed step 2: exit 70 instead of 0; stdout did not match `/approved greet [0-9a-f]{8} \(approval [0-9a-f]{8}\); it is queued\n/`; observed stdout bytes were `controller/internal_error: command handler is not available\n`. `v1.2-04` similarly failed its approval preflight, so its key/header/journal assertions were not reached. `provider-24` failed the shape command in all 10 rows. The dependency command handlers are outside this package checkout; do not mark package 10 done until those dependencies are merged and this exact command is rerun.

`provider-05`'s own assertion, which was not reached, requires each key to equal `sha256("kogen:responses:v1\0" + run dir + "\0" + stage)`. That stage-scoped v1.1 rule conflicts with v1.2 §4.9.1: one stable Build-affinity key across stages, with a separate stage/attempt/rung/epoch thread id. The implementation follows §4.9.1 and `v1.2-04`. Request a versioned correction to the read-only conformance source; do not count the old assertion as a pass.

Other v1.1 assertions compared with §4.5 before making any compatibility claim:

- `provider-14` asks for a retry after a 200 s first byte; this exceeds v1.2's 120 s first-byte deadline, so the policy is compatible. The case itself did not reach the provider.
- `provider-15` expects `timeout` for 200 s idle gaps. v1.2 uses a 90 s idle deadline and classifies silence after bytes as `stall`.
- `provider-16` expects a timeout after an 800 s stream under a 600 s total cap. v1.2's per-attempt cap is 1,200 s, so 800 s is below the deadline.
- `provider-13` expects planner fallback after two 503s; §4.5 says the planner has no fallback.
- `provider-23` expects the retired 10,000-character shell tail and the old non-UTF-8 “tail” marker. v1.2 §4.9.3 uses the configured token/byte budget and the whole-payload base64 marker.
- `provider-17` expects transport after a stream drop. A reset maps to transport under §4.4; a clean EOF without `response.completed` is malformed under §4.3. The suite did not reach either path.

Request versioned replacements or corrections for the superseded `provider-05`, `provider-13`, `provider-15`, `provider-16`, and `provider-23` assertions. The supplied conformance tree remained read-only.

### Live cache gate

The credential-safe live three-turn >95% warm-cache smoke was not run. The temporary HOME had no credentials, and the current public binary cannot reach its Build request path. No real credentials or live provider were used. This release gate remains unmet; there are no warm numerators/denominators or missing-usage observations to report.
