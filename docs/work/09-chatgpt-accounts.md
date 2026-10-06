# 09 ChatGPT account ownership (G, 4–6 h)

Own `kogen-core::provider::auth` and `chatgpt` credential/account modules, plus CLI provider handlers. Depends on 01. Implement `list`, ChatGPT `login`, `logout`, `use --as`, file seam and macOS/Linux secret backend, injected auth validation, PKCE/OAuth discovery/callback/JWKS, refresh lock and one-account-per-run resolution. Use spec §1.7.6, §2.7, §4.6. Never read another agent's login. No credentialed request in unit or replay tests.

Acceptance: `provider-10`, `provider-19`, `provider-21`, `provider-22`, `state-14`, `format-11`; `provider-02` becomes complete when package 10 sends a request. Quint: none mandatory; `accounts` all 9 hand scenarios and 500 × 25 seeds 17, 23, 41 are diagnostic because cross-provider precedence is L. Verify fake OAuth and `KOGEN_CREDENTIAL_STORE=file` with a temporary HOME.
