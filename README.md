# memcastle-slack

[![CI][ci-badge]][ci-workflow] [![Codecov][codecov-badge]][codecov-project]

[ci-badge]: https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml/badge.svg?branch=main
[ci-workflow]: https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml
[codecov-badge]: https://codecov.io/gh/memcastle/memcastle-slack/graph/badge.svg
[codecov-project]: https://codecov.io/gh/memcastle/memcastle-slack

Standalone plugin for explicitly scoped Slack conversation and thread history.

The `slack` source supports independently selected bot and user Web API tokens from named source configurations;
credentials remain outside plugin configuration and memory. It mines only configured channel, private-channel,
DM or group-DM IDs. Each thread has a stable document with Core-managed revisions, cursors and destinations;
deletions are not propagated by Core. Use a Core build with [PR #335](https://github.com/memcastle/memcastle/pull/335).
See [the source guide](sources/slack/README.md) for app scopes, installation, accounts and miner routing.

## Install

When a release is available in the [MemCastle plugin catalogue](https://memcastle.github.io/plugins.json),
install it by ID with a plugin-capable MemCastle Core binary:

```sh
memcastle plugin install slack
memcastle source enable slack
```

Core retrieves and verifies the release archive. Installation alone does not activate the source or start a miner;
configure a named source and scoped miners as shown in the source guide. If no release is available yet,
build the archive below and install `dist/slack-VERSION.tar.gz` instead.

## Build and test locally

Install Rust with the `wasm32-wasip2` target and a plugin-capable MemCastle CLI. Use
`MEMCASTLE_BIN=/path/to/current/memcastle` when the default CLI predates the selected-credential contract.
Commit `sources/slack/Cargo.lock` for reproducible builds.

```sh
cargo fmt --manifest-path sources/slack/Cargo.toml --all -- --check
cargo test --manifest-path sources/slack/Cargo.toml --locked
cargo clippy --manifest-path sources/slack/Cargo.toml --locked --target wasm32-wasip2 -- -D warnings
cargo build --manifest-path sources/slack/Cargo.toml --locked --release --target wasm32-wasip2
cp sources/slack/target/wasm32-wasip2/release/slack.wasm sources/slack/source.wasm
mkdir -p sources/slack/dist
cp sources/slack/source.wasm sources/slack/dist/source.wasm
PATH="$PWD/sources/slack/fixtures/bin:$PATH" memcastle source test sources/slack --no-build
memcastle plugin inspect --json
memcastle plugin package
memcastle plugin test dist/slack-VERSION.tar.gz
node scripts/smoke_miners.mjs
```

Replace `VERSION` with the archive version in `dist/`. Packaging includes the source component and a generated
`manifest.toml` with an external `.sha256sum`; `plugin.toml` retains the stable module identity and paths.

## CI and releases

CI validates the exact artifact against Core `development` and `0.5` with the shared plugin-validation workflow.
The Core setup action checks the binary against `SHA256SUMS`; an extra offline smoke covers miners sharing
one bot account, a second bot account and a user file credential. No live Slack workspace is needed.

Native Rust tests upload LCOV coverage and JUnit results to Codecov via GitHub OIDC.
The repository's `.github/codecov.yml` defines the project, patch and source-module checks.

gh-ship and git-cliff open a reviewed `release/next` PR for releasable commits. After it merges,
the validated `slack/vVERSION` archive is published. Configure `APP_CLIENT_ID` and `APP_PRIVATE_KEY` in
the `release` environment for an installed GitHub App.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and [AGENTS.md](AGENTS.md) for agent guidance.
