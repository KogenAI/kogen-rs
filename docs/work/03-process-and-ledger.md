# 03 Process custody and acceptance ledger (G, 4–6 h)

Own `kogen-core::run` process supervisor and `gate::ledger` submodule. Depends on no package. Implement argv-only child spawning, separate process groups, deadline, TERM/KILL/reap, output log/tail, private script transport for the shell tool, filtered child environment and mise seam, and command acceptance adapter with JSONL ledger. Use spec §2.4 and §5.1–5.3. Expose a port so approval and gate use identical runner semantics. Leave Git landing to package 08.

Acceptance after the approval/build callers are wired: `approval-10`, `approval-11`, `approval-12`, `approval-13`, `approval-19`, `build-11`, `build-12`, `build-13`, `build-14`, `build-18`, `build-19`, `build-20`, `build-39`, `build-40`, `custody-01`, `custody-02`, `custody-03`, `custody-04`, `custody-06`, `custody-07`, `custody-08`. Ownership of those behaviors remains here even when a later package first makes the cases runnable. Quint: none; no custody or ledger slice exists. Unit verification must include a chatty deadline child and a grandchild cleanup, plus ledger absence, malformed row and tree mutation.

## Verification record (2026-10-06)

- Source baseline SHA: `08350a1eaead9096a5f149ec96e7d54e8138dfc3`.
- Conformance source: read-only `v1.2`, SHA `c1907685e52b9ba965f540b6cc162b489d4f4334`.
- `target/debug/kogen` SHA-256: `963462b54a44003f26f6c460fe0717f60a58a242f9a52fb5ec8eb5a1221c76cd`.
- `make check`: pass (format, clippy with warnings denied, and 21 unit tests). `cargo check --workspace --target x86_64-unknown-linux-musl`: pass.
- Conformance command:

  ```sh
  /Users/almirsarajcic/Areas/Kogen/kogen-conformance/bin/kogen-conformance run \
    --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/03-process-and-ledger/target/debug/kogen \
    --case 'approval-10,approval-11,approval-12,approval-13,approval-19,build-11,build-12,build-13,build-14,build-18,build-19,build-20,build-39,build-40,custody-01,custody-02,custody-03,custody-04,custody-06,custody-07,custody-08' \
    --jobs 8 \
    --workdir /tmp/kogen-03-process-ledger-conformance \
    --keep \
    --out /tmp/kogen-03-process-ledger-results.jsonl \
    -v
  ```

- Case result: pass IDs: none. Fail IDs: `approval-10`, `approval-11`, `approval-12`, `approval-13`, `approval-19`, `build-11`, `build-12`, `build-13`, `build-14`, `build-18`, `build-19`, `build-20`, `build-39`, `build-40`, `custody-01`, `custody-02`, `custody-03`, `custody-04`, `custody-06`, `custody-07`, `custody-08`. All 21 stop before exercising these seams: the current `kogen` skeleton exits 70 and writes the exact stderr bytes `kogen CLI is not implemented yet\n`. No case was skipped or counted as a pass.
- Quint hand scenarios: 0; trace seeds/steps: none required because this package has no Quint slice.
