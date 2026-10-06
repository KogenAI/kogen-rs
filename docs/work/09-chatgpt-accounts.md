# 09 ChatGPT account ownership (G, 4–6 h)

Own `kogen-core::provider::auth` and `chatgpt` credential/account modules, plus CLI provider handlers. Depends on 01. Implement `list`, ChatGPT `login`, `logout`, `use --as`, file seam and macOS/Linux secret backend, injected auth validation, PKCE/OAuth discovery/callback/JWKS, refresh lock and one-account-per-run resolution. Use spec §1.7.6, §2.7, §4.6. Never read another agent's login. No credentialed request in unit or replay tests.

Acceptance: `provider-10`, `provider-19`, `provider-21`, `provider-22`, `state-14`, `format-11`; `provider-02` becomes complete when package 10 sends a request. Quint: none mandatory; `accounts` all 9 hand scenarios and 500 × 25 seeds 17, 23, 41 are diagnostic because cross-provider precedence is L. Verify fake OAuth and `KOGEN_CREDENTIAL_STORE=file` with a temporary HOME.

## Verification record

- Implementation source SHA: `f6a21187b86ef594631666295b8aa8beb541e235`.
- `target/debug/kogen` SHA-256: `af9ea02ca5ae85c898da781e747a230bed3575611d4c535440122e88eed12d04`.
- Spec checkout SHA: `1118f7fc0dbb042af8c8de2ffd1b85768cf9e2a0`.
- Conformance checkout SHA: `c1907685e52b9ba965f540b6cc162b489d4f4334`.
- `make check`: pass. Four core unit tests pass; CLI and xspec have no unit tests.
- Fake OAuth with temporary `HOME`, `KOGEN_AUTH_URL` fake endpoint, and `KOGEN_CREDENTIAL_STORE=file`: pass. Checked PKCE S256, JWKS signature and claims, token revocation, credential mode `0600`, list/use/logout, and two immediate callbacks on port 1455. Fake provider request count: 0.

Exact conformance command:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen "$PWD/target/debug/kogen" --profile provider,state,format --case 'provider-02,provider-10,provider-19,provider-21,provider-22,state-14,format-11' --jobs 1 --out /tmp/kogen-09-accounts-final-results.jsonl --verbose
```

Result: `format-11` PASS. `provider-02`, `provider-10`, `provider-19`, `provider-21`, `provider-22`, and `state-14` FAIL; 1 pass, 6 fails across 9 instances. `provider-19`, `provider-10`, `provider-02`, and `provider-22` stop at the missing intent/approval handlers (`controller/internal_error: command handler is not available`, exit 70), before any provider request. These require neighboring packages; `provider-02` also requires package 10's request implementation.

The v1.1 expectations in `provider-21` and `state-14` expect the single line `chatgpt: not signed in\n` when no profile is present. Current spec §1.7.6 requires a row for each provider; observed bytes are `chatgpt: not signed in\ngrok: not signed in\n`. This is the v1.1/v1.2 boundary. A versioned conformance correction is needed to update those v1.1 expectations; the read-only conformance checkout was not changed. These two failures are not counted as passes.

Diagnostic `accounts` Quint slice: the spec selfcheck accepts all 9 hand scenarios. Generated invariant checks pass for 500 traces × 25 steps at each seed 17, 23, and 41 (12,500 events per seed). For each seed, full xspec adapter conformance is 0/509 traces, 0 steps: the first divergence is the initial reset, where `kogen-xspec` returns `adapter error: adapter gave no answer to {"op": "reset"} kogen-xspec replay is not implemented yet`. The adapter is outside this package; no projection or skipped case was counted as a pass.

Harness invocations used for the diagnostic slice (with the account slice copied to a temporary directory so the spec checkout remains untouched):

```sh
python3 ../../kogen-spec/quint/prototype/harness/xspec.py spec
python3 ../../kogen-spec/quint/prototype/harness/xspec.py gen --traces 500 --steps 25 --seed 17
HOME="$TRACE_ROOT/home" python3 ../../kogen-spec/quint/prototype/harness/xspec.py conform -- "$PWD/target/debug/kogen-xspec" accounts
```

The `gen` and `conform` commands were repeated with seeds `23` and `41`.
