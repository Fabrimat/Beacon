# Beacon

Beacon is an open-source Rust toolchain for Minecraft Bedrock and Education
add-ons: validation, packaging and performance checks for behavior and resource
packs.

**Status:** pre-alpha, phase 0. Interfaces and layout will change.

## Docs

- [Architecture](docs/ARCHITECTURE.md)
- [Backlog](docs/BACKLOG.md)

## Development

```sh
cargo test --workspace
cargo corpus --size small --out target/corpus-small
cargo spike
cargo perf run --beacon target/release/beacon --project target/corpus-small
```

On Windows the beacon binary is `target/release/beacon.exe`.

The Rust toolchain is pinned in `rust-toolchain.toml`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports: [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Beacon is not affiliated with, endorsed by, or sponsored by Mojang Studios or
Microsoft.

Beacon collects no telemetry.
