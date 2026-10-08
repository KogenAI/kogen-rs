# Kogen in Rust

> **Archived.** Kogen in Rust, built from [kogen-spec](https://github.com/KogenAI/kogen-spec) between 6 and 8 October 2026, when we built Kogen in Rust, Go and TypeScript from the same specification to choose a stack. It passes all 278 cases of the [kogen-conformance](https://github.com/KogenAI/kogen-conformance) v1.3-draft suite. Rust was chosen; Kogen continues in Rust in a new repository. More at [kogen.dev](https://kogen.dev).
>
> The comparison: [kogen-spec](https://github.com/KogenAI/kogen-spec) · [kogen-conformance](https://github.com/KogenAI/kogen-conformance) · [kogen-rs](https://github.com/KogenAI/kogen-rs) · [kogen-go](https://github.com/KogenAI/kogen-go) · [kogen-ts](https://github.com/KogenAI/kogen-ts)

Kogen turns a software request into a reviewed Intent and acceptance tests, waits for explicit approval, builds approved work in an isolated workspace, verifies the result, and lands changes through Git.

This repository is the Rust implementation of the **Kogen specification v1.3-draft**. The command grammar is fixed. The companion repositories are [Kogen specification](https://github.com/KogenAI/kogen-spec) and [Kogen conformance suite](https://github.com/KogenAI/kogen-conformance).

**Conformance status:** v1.3, 278/278 cases passed. See [validation details](docs/CONFORMANCE.md).

## Requirements

- Rust 1.97.1, installed with rustup. The repository pins this version.
- Git, used for project discovery and change publication.
- A native C compiler and linker for dependencies.
- The target project's own toolchain and commands for checks, such as Cargo, Mix, or Bundler.

## Build and install

From a Git checkout:

~~~sh
cargo build --locked --release -p kogen
cargo install --locked --path crates/kogen-cli
~~~

Building from a Git checkout gives the executable its source revision and build date. Builds from source archives without Git metadata use the fallback version 00000000 and date 1970-01-01.

## Project setup

Kogen looks for .kogen/project.yaml at the root of a Git project. This minimal example is valid against the project configuration schema:

~~~yaml
name: example
checks:
  - name: tests
    argv: ["cargo", "test", "--locked"]
~~~

Replace the example check with commands appropriate for the project. Kogen does not provide an init command.

## Fixed CLI workflow

~~~text
kogen help
kogen provider login chatgpt
kogen intent shape greeting request.md
kogen intent approve greeting
kogen intent approve greeting <reviewed-hash>
kogen queue start
kogen status
~~~

Shaping writes an Intent and its acceptance source for review. The first approval command prints the review card and exits with code 5. Review the card, then pass its displayed hash to the second approval command. Approval adds the Intent to the queue; queue start starts processing it.

ChatGPT and Grok provider implementations are included. The conformance run uses fake providers and does not measure live provider availability or performance.

## Platforms and verification

The runtime has macOS and Linux confinement implementations. macOS uses sandbox-exec; Linux uses bwrap when available. If the platform confinement tool is unavailable, Kogen warns and follows the implemented unconfined fallback. This candidate was exercised on macOS; its x86_64 Linux target was compile-checked, but Linux runtime confinement was not exercised.

Run the local checks with:

~~~sh
make check
~~~

The current conformance result and its scope are documented in [docs/CONFORMANCE.md](docs/CONFORMANCE.md). Build and install commands use the workspace lockfile.

## License

Kogen is licensed under [Apache License 2.0](LICENSE).
