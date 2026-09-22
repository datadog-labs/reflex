# Contributing to Reflex

Reflex is a Rust library for AI-guided state machines. Contributions can improve
the SDK, integrations, simulations, documentation, or tests.

Start with the [README](README.md) for the library's purpose and examples, and
the [SDK guide](SDK_README.md) for its APIs and execution behavior.

## Propose a feature

Before implementing a feature or a substantial change, open an
[issue](https://github.com/datadog-labs/reflex/issues) titled
`[RFC] Your proposal` and discuss it with the maintainers. Include:

- The problem and who encounters it.
- The proposed behavior and a usage example.
- Your implementation approach, alternatives, and compatibility implications.

Wait for maintainer approval before starting implementation. Link the approved
proposal in your pull request. Small documentation corrections and straightforward
bug fixes do not need an RFC.

## Report or fix a bug

Search existing issues before opening a new one. A useful bug report includes:

- The affected commit or version, operating system, and relevant tool versions.
- Steps to reproduce, ideally with a minimal example.
- Expected and actual behavior.
- Relevant logs or error messages, with credentials and private data removed.

For simulation bugs, include the scenario, policy, seed, controls you changed,
and whether live integrations were enabled.

If you want to fix an existing issue, comment on it first to coordinate with the
maintainers and other contributors. Discuss substantial changes through an RFC.
Link your pull request to the issue it addresses.

## Set up your development environment

Install Git, Rust 1.92 or newer, and Node.js with npm. Node.js and npm are needed
to build the simulator UI before building or testing the full Rust workspace.

```sh
git clone https://github.com/datadog-labs/reflex.git
cd reflex
rustup component add rustfmt clippy
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
```

Use a fork if you do not have permission to push branches to the repository.

The UI build generates assets that Cargo embeds in the simulator. Rebuild those
assets after UI changes; generated bundles are ignored by Git.

Run the circuit-breaker SDK example without service credentials:

```sh
cargo run -p reflex --locked --example circuit_breaker
```

For simulator development, see the [playground guide](crates/reflex-sim/README.md)
and [UI build instructions](crates/reflex-sim/ui/README.md). Research tooling has
separate setup instructions in the [capacity study guide](studies/capacity/README.md).

Live Jev calls require `TYPESAFE_API_KEY` and may incur charges. Datadog integrations
require their own configuration. Never commit credentials or put them in issues,
logs, screenshots, or test fixtures.

## Test and lint your changes

From the repository root, after building the UI:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

Use `cargo fmt --all` to apply Rust formatting. Add tests for changed behavior,
including relevant rejection and failure paths. Prefer deterministic tests that
do not require external services. Live integration tests must be explicitly run
with the required credentials; do not enable them for routine test runs.

For UI changes, rebuild the UI and check the affected simulation in the browser.
Verify desktop and narrow layouts, keyboard interaction, navigation, playback,
node selection, controls, and charts where your change affects them. Include
screenshots or a short recording when they help reviewers assess the change.

## Follow the execution model

Keep guards and state updates short and free of external side effects. Put
network requests and other I/O in declared effects. Preserve the separation
between a model recommendation and a committed transition.

Keep changes focused. Update examples and documentation when public behavior
changes. Explain compatibility changes and new dependencies in the pull request,
including their licenses and any required notices.

## Sign your commits

Datadog requires contributors to sign their commits. Follow
[GitHub's commit-signing instructions](https://docs.github.com/en/authentication/managing-commit-signature-verification/signing-commits)
to configure a signing key. With signing configured:

```sh
git commit -S -m "Describe the change"
```

A `Signed-off-by` trailer alone is not a cryptographic commit signature.

## Submit a pull request

Open a pull request against `main`. Explain the problem, the resulting behavior,
and how you validated the change. Link related issues or approved RFCs, and
identify any tests you could not run.

Keep unrelated cleanup separate. Respond to review feedback and keep code,
examples, and documentation consistent as the change evolves.

## License

Contributions must be compatible with Apache-2.0. By contributing to Reflex,
you agree to license your contributions under Apache-2.0.

## Use of AI coding assistants

AI coding assistants are welcome. The same proposal, review, signing, and testing
requirements apply regardless of the tools used.

You are responsible for understanding and reviewing the code you submit,
verifying its behavior, and checking any introduced dependencies or copied code.
Maintainers may close contributions that have not been reviewed or tested by the
contributor. Do not submit generated changes you cannot explain.

## Help triage issues

You can contribute without changing code:

- Reproduce reported bugs and share your environment and results.
- Link duplicate issues to the original report.
- Ask for missing reproduction steps or version information.
- Check whether documentation answers a question and suggest improvements.
