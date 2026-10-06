# Package 15 verification

Implementation source commit: 7100a5768fa48b2dac2339d57814a343717da79f

Binary SHA-256:

- kogen: 8c069b0b542552e966052e0e92e70c3675e66246d1ce3e740e859118aa1ea842
- kogen-xspec: ffb0ee8bf888e139749a0a1e221edaab1f3a30c0910f37b00aa18d000e83ccda

## Checks

- make check: pass.
- cargo test -p kogen-core gate::adapters: 11 passed.
- Elixir 1.20.4 was installed; no ExUnit case was skipped.
- Proposed P12 has no named suite file. Its two-file rule, paths, and runner are covered by the passing Rails adapter unit tests.

Exact black-box command:

    /Users/almirsarajcic/Areas/Kogen/kogen-conformance/bin/kogen-conformance run --kogen /Users/almirsarajcic/Areas/Kogen/kogen-rs-wt/15-stack-adapters/target/debug/kogen --case 'exunit-01,exunit-02,exunit-03,exunit-04,exunit-05,exunit-06,state-03,build-14,build-19' --jobs 1 --out target/adapter-conformance-results-7100a57.jsonl -v

Result: 0 passed, 9 failed, 0 skipped. Pass IDs: none. Fail IDs: state-03, build-14, build-19, exunit-01, exunit-02, exunit-03, exunit-04, exunit-05, exunit-06.

- state-03 step 2: exit 70 instead of 0; stdout was controller/internal_error: command handler is not available, instead of the expected three status lines beginning Queue: stopped.
- build-14 and build-19 step 3: queue start returned controller/internal_error: command handler is not available; the harness could not find a run for greet.
- exunit-01 and exunit-06 step 3, exunit-03 step 3, and exunit-02 step 4: queue start returned controller/internal_error: command handler is not available (exit 70; exunit-02 expected exit 3).
- exunit-04 step 3: approval JSON assertion found no check_baseline entry with name unit, status red, and a finding at test/hello_test.exs whose symbol matches legacy is broken.
- exunit-05 step 2: shaping returned provider/malformed (HTTP 400), instead of the expected successful shape result. The provider diagnostic shows the shaper prompt asking for .kogen/acceptance/greet.t.sh, although this frozen v1.1 case sets acceptance_ext to _test.exs, matching v1.2 §2.4.3. This is an adapter-selection integration gap, not a superseded case assertion; the v1.2 suite has no separate ExUnit case file.

The command handler and shaping mismatch arise before the new stack adapters are called. The six ExUnit and two build cases require the absent production queue/build integration; exunit-04 also requires that integration to derive the approval baseline.

## Quint gate slice (diagnostic)

The spec and five checked-in hand scenarios were copied to target/adapter-quint-gate-76_aaml5 so the read-only spec checkout and its golden expectations stayed untouched.

- Quint spec command: XSPEC_SLICE=target/adapter-quint-gate-76_aaml5 python3 /Users/almirsarajcic/Areas/Kogen/kogen-spec/quint/prototype/harness/xspec.py spec
- Spec result: all 5 hand scenarios agree with the Quint spec.
- Full-observation xspec hand replay: 0/5 traces agree.
- Generated traces: seeds 17, 23, and 41; each generated 500 traces × 25 steps (12,500 events), with spec invariants holding. Full-observation replay was 0/500 for each seed.
- First divergence for every hand/generated trace: step 0, reset; kogen-xspec exits with unsupported slice gate, so it returns no observation. No projected comparison was used.
