# Public CLI verification record

Tested implementation commit: `b6640297e30e35ed0a3ff4836d350370f5cf6a16`.
Tested binary: `target/debug/kogen`, SHA-256
`66c116da88068fa2d0c3072b605d0717e1c7f149452bdeddfdd9103d20afaed4`.
Spec source: `kogen-spec` `1118f7fc0dbb042af8c8de2ffd1b85768cf9e2a0`.
Conformance source: `kogen-conformance` `c1907685e52b9ba965f540b6cc162b489d4f4334`.

`spec/data/help/` contains 15 files at the recorded spec revision. The frozen
v1.1 conformance data contains a sixteenth, historical `kogen-help.txt`; it is
not present in the current spec corpus. All 15 available pages were copied
byte-for-byte and embedded.

## Commands and results

`make check` passed: formatting, clippy with warnings denied, and workspace
tests (1 unit test passed).

The exact case command was:

```sh
../../kogen-conformance/bin/kogen-conformance run --kogen "$PWD/target/debug/kogen" --profile cli,format,v1.2 --case 'cli-01,cli-02,cli-03,cli-04,cli-05,cli-06,cli-07,cli-08,cli-09,cli-10,cli-11,cli-12,cli-13,cli-14,cli-15,cli-16,cli-17,cli-18,cli-19,cli-20,cli-21,cli-22,cli-23,cli-24,format-01,format-06,format-09,v1.2-01-fixed-cli-help-and-grok' --jobs 1 --out /tmp/kogen-cli-01-committed-results.jsonl
```

Whole-case passes: `cli-10`, `cli-18`, `cli-19`, `cli-20`, `cli-22`,
`format-06`.

Whole-case failures: `cli-01` through `cli-09`, `cli-11` through `cli-17`,
`cli-21`, `cli-23`, `cli-24`, `format-01`, `format-09`,
`v1.2-01-fixed-cli-help-and-grok`.

The run had 85 passing and 132 failing instances out of 217; there were no
skips or harness errors. The v1.2 case passed 18/21 instances: its 18 current
help-page and route rows pass. The final three provider action rows reach the
typed dispatch boundary and currently return
`controller/internal_error: command handler is not available`.

## Frozen v1.1 conflicts and missing handlers

The required v1.2 help corpus and route rules disagree with frozen v1.1 help
expectations. For `cli-02` and `format-01`, the first row expects
`kogen help help` to exit 0 with the historical `kogen-help.txt`. The exact
observed stdout bytes are:

```text
kogen help: unexpected argument 'help'

Commands:
  status     Show the queue, Builds and Intents
  intent     Shape, approve or remove an Intent
  queue      Build approved Intents one at a time
  provider   Manage Kogen's provider logins
  version    Show the Kogen version
  help       List commands

Intent shaping defaults to gpt-6.1-sol at high effort; an explicit build.roles.shaper
in project or machine config overrides this default.

Run kogen <command> to see its subcommands and options.
```

It exits 2, as specified for `help` with an additional argument. The current
spec has no `kogen-help.txt` page or help-topic route.

For `cli-11`, the first row expects the v1.1 provider list and help page. The
actual stdout bytes for `provider login github` are:

```text
kogen provider login: unknown provider 'github' (supported: chatgpt, grok)

Usage: kogen provider login <provider>

Supported providers: chatgpt, grok.
```

The CLI-RULE addendum and v1.2 spec admit `grok`; the v1.1 row expects only
`chatgpt`. `v1.2-01-fixed-cli-help-and-grok` is the required exact current help
oracle and confirms the current provider name and current page bytes.

Other frozen grammar conflicts include `--help` as a help shortcut (`cli-01`,
`cli-24`), help topics (`cli-17`), and parser-time slug-shape errors
(`cli-13`, `cli-21`). The current §1.3 says `--help` is not special and the
parser leaves slug shape to the command handler.

Action rows depend on core packages whose handler modules are still stubs in
this worktree: project/status (`cli-15`, `cli-23`, and the project rows of
`format-09`), intent actions (`cli-13`, `cli-21`, and the remaining
`format-09` rows), and provider/account actions (the final three rows of the
v1.2 case). They are not counted as passes.

No conformance case, golden page, or Quint expectation was edited. A versioned
correction is requested in the read-only conformance source for superseded
v1.1 assertions; the current v1.2 case remains the help oracle.

## Quint

This package has no Quint slice: hand scenarios `0`, generated traces `0`,
seeds and steps `none`, first divergence `none` (not applicable).
