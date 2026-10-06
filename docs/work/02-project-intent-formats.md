# 02 Project and Intent formats (G kernel, 4–6 h)

Own `kogen-core::project` and `intent`, including strict YAML reader, project resolution, slug, frontmatter, raw Request preservation, lint error kernel and approval byte inputs. Depends on no package. Use spec §1.6 and §2.1–2.4. Preserve Intent and acceptance bytes; never normalize before hashing. The broader style phrase tables are L work in package 12.

Acceptance: `state-01`, `state-02`, `state-03`, `state-04`, `state-05`, `state-07`, `state-13`, `state-16`, `approval-21`, `cli-21`, `cli-23`, `format-05`. `state-08` and `v1.2-02-approval-hash-intent-and-test-bytes` become complete with package 04. Quint: none yet; package 04 owns the `intent` replay lifecycle so it can use real refs. Test the first error line and class in the YAML corpus, not merely parser success.

## Implementation and verification record

- Source revision: `0d777d2634e8054af06f825601d5a81eb3761420`.
- Binary SHA-256: `01492f5bf01059ddb4667aefd98f6fa098ec4f0da5eeacb8d6249d57459ceb56` (`target/debug/kogen`, built from the source revision above).
- `make check`: pass (formatting, clippy with warnings denied, and all 13 `kogen-core` tests).
- Conformance command: `../../kogen-conformance/bin/kogen-conformance run --kogen target/debug/kogen --profile cli,state,approval,format --case 'state-01,state-02,state-03,state-04,state-05,state-07,state-13,state-16,approval-21,cli-21,cli-23,format-05' --workdir /tmp/kogen-p02-final-conformance --out /tmp/kogen-p02-final-conformance/results.jsonl --keep -v`.
- Conformance result: 0 passed, 12 failed, 0 skipped, 0 errors; 90 instances. Pass IDs: none. Fail IDs: `state-01`, `state-02`, `state-03`, `state-04`, `state-05`, `state-07`, `state-13`, `state-16`, `approval-21`, `cli-21`, `cli-23`, `format-05`. The first observed command failure is that the current `kogen` executable returns 70 with empty stdout and stderr `kogen CLI is not implemented yet\n`; the cases require the public CLI to dispatch these commands. The core implementation is therefore awaiting CLI integration before these acceptance cases can pass.
- Quint: no slice is assigned to this package. Hand scenarios: 0. Trace seeds/steps: not applicable. First divergence: not applicable.
