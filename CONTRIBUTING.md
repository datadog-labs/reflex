# Contributing

Thanks for your interest in Reflex! This document describes how to report
issues and submit changes.

## Submitting issues

- **Bugs:** open a [bug report](https://github.com/datadog-labs/reflex/issues/new?template=bug_report.md) with
  reproduction steps, the crate and version involved, and your environment.
- **Feature requests:** open a [feature request](https://github.com/datadog-labs/reflex/issues/new?template=feature_request.md)
  describing the problem you are trying to solve.
- **Security issues:** do not open a public issue. Follow the
  [Datadog security policy](https://github.com/DataDog/.github/blob/master/SECURITY.md) instead.

## Development setup

Reflex is a Rust workspace (see `rust-version` in [Cargo.toml](Cargo.toml) for
the minimum supported toolchain). The simulator UI in `crates/reflex-sim/ui`
also needs Node.js and npm.

Build the UI assets, then run the full test suite:

```sh
npm ci --prefix crates/reflex-sim/ui
npm run build --prefix crates/reflex-sim/ui
cargo test --workspace --all-targets --locked
cargo test --workspace --doc --locked
```

Compile-fail tests in `crates/reflex/tests/ui` compare compiler output against
`.stderr` snapshots. If you intentionally change a diagnostic, regenerate them
with `TRYBUILD=overwrite cargo test -p reflex --test compile_fail` and review
the diff.

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Keep changes focused, and add or update tests for new behavior.
3. Run `cargo fmt --all` and `cargo clippy --workspace --all-targets` and make
   sure the test suite above passes.
4. Add the license header used throughout the repository to new source files.
5. If you add, remove, or upgrade a dependency, update the third-party inventory
   as described in [third_party/README.md](third_party/README.md) and check it
   with `python3 scripts/update_third_party.py --check`.
6. Open a pull request describing what changed and why.

## License

By contributing to this repository, you agree that your contributions will be
licensed under the [Apache-2.0 License](LICENSE).
