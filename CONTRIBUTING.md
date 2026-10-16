# Contributing to Beacon

Thanks for helping. Beacon is licensed under Apache-2.0.

## Developer Certificate of Origin

All commits must carry a DCO sign-off. Use `git commit -s`, which appends:

```
Signed-off-by: Your Name <you@example.com>
```

By signing off you certify the [Developer Certificate of Origin](https://developercertificate.org/).

## Before opening a PR

Run these locally; CI runs the same checks on Windows and macOS:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Production projects

Production add-on projects must never be added to this repository, its
fixtures, or CI. Benchmarks and tests use the generated corpus only. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) section 10.
