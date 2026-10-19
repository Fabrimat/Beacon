# Beacon

Beacon is an open-source Rust toolchain for Minecraft Bedrock and Education
add-ons: validation, packaging and performance checks for behavior and resource
packs.

**Status:** pre-alpha, phase 0. Interfaces and layout will change.

## Docs

- [Architecture](docs/ARCHITECTURE.md)
- [Backlog](docs/BACKLOG.md)

## Install (alpha, Windows)

Alpha builds are **unsigned** and not published: the Windows installer comes from a link
someone gives you. Windows shows "Windows protected your PC": click **More info**, then **Run
anyway**. No administrator rights are needed.

Requirements: Windows 11, and Minecraft (Bedrock, from the Store) started once, signed in, and
left at the main menu. Then open Beacon, click Continue on Setup, create a project and click
Play. Step-by-step test script: [docs/alpha-test.md](docs/alpha-test.md).

## Development

```sh
cargo build --release -p beacon-cli
beacon=target/release/beacon        # beacon.exe on Windows
$beacon build --project fixtures/minimal --target addon --trust

cargo test --workspace
cargo corpus --size large --out target/corpus-large
cargo spike
cargo perf run --beacon $beacon --project target/corpus-large --runs 20 --enforce --phase 1
```

Script bundling needs a native [esbuild](https://esbuild.github.io/) binary (0.28.2 is what CI
uses). Beacon looks for it in `BEACON_ESBUILD`, then `node_modules/@esbuild/<platform>`, then
`PATH`; point `BEACON_ESBUILD` at the platform-native binary (not an npm `.cmd` shim), e.g.
`npm i --prefix .ci-esbuild esbuild@0.28.2` and
`.ci-esbuild/node_modules/@esbuild/win32-x64/esbuild.exe`. The perf harness always passes
`--trust` and inherits `BEACON_ESBUILD`.

The Rust toolchain is pinned in `rust-toolchain.toml`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security reports: [SECURITY.md](SECURITY.md).

## License

Apache-2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

Beacon is not affiliated with, endorsed by, or sponsored by Mojang Studios or
Microsoft.

Beacon collects no telemetry.
