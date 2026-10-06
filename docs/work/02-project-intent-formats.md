# 02 Project and Intent formats (G kernel, 4–6 h)

Own `kogen-core::project` and `intent`, including strict YAML reader, project resolution, slug, frontmatter, raw Request preservation, lint error kernel and approval byte inputs. Depends on no package. Use spec §1.6 and §2.1–2.4. Preserve Intent and acceptance bytes; never normalize before hashing. The broader style phrase tables are L work in package 12.

Acceptance: `state-01`, `state-02`, `state-03`, `state-04`, `state-05`, `state-07`, `state-13`, `state-16`, `approval-21`, `cli-21`, `cli-23`, `format-05`. `state-08` and `v1.2-02-approval-hash-intent-and-test-bytes` become complete with package 04. Quint: none yet; package 04 owns the `intent` replay lifecycle so it can use real refs. Test the first error line and class in the YAML corpus, not merely parser success.
