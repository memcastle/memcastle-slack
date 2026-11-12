# memcastle-slack

[![CI][ci-badge]][ci-workflow] [![Codecov][codecov-badge]][codecov-project]

[ci-badge]: https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml/badge.svg?branch=main
[ci-workflow]: https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml
[codecov-badge]: https://codecov.io/gh/memcastle/memcastle-slack/graph/badge.svg
[codecov-project]: https://codecov.io/gh/memcastle/memcastle-slack

Standalone plugin for explicitly scoped Slack conversation and thread history.

The `slack` source supports independently selected bot and user Web API tokens from named source configurations;
credentials remain outside plugin configuration and memory. It mines only configured channel, private-channel,
DM or group-DM IDs. Each thread has a stable document with Core-managed inventory, revisions, retirement,
restoration, cursors and destinations. Use development Core containing
[`c311d09`](https://github.com/memcastle/memcastle/commit/c311d09) (Core #378) or a verified descendant.
Older stable binaries also labeled `0.5.2` do not support the reconciliation extension.
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
`MEMCASTLE_BIN=/path/to/current/memcastle` to select a reconciliation-capable CLI.
Commit `sources/slack/Cargo.lock` for reproducible builds.

```sh
cargo fmt --manifest-path sources/slack/Cargo.toml --all -- --check
cargo test --manifest-path sources/slack/Cargo.toml --locked
cargo clippy --manifest-path sources/slack/Cargo.toml --locked --target wasm32-wasip2 -- -D warnings
cargo build --manifest-path sources/slack/Cargo.toml --locked --release --target wasm32-wasip2
cp sources/slack/target/wasm32-wasip2/release/slack.wasm sources/slack/source.wasm
mkdir -p sources/slack/dist
cp sources/slack/source.wasm sources/slack/dist/source.wasm
PATH="$PWD/sources/slack/fixtures/bin:$PATH" "${MEMCASTLE_BIN:-memcastle}" source test sources/slack --no-build
"${MEMCASTLE_BIN:-memcastle}" plugin inspect --json
"${MEMCASTLE_BIN:-memcastle}" plugin package
"${MEMCASTLE_BIN:-memcastle}" plugin test dist/slack-VERSION.tar.gz
node scripts/smoke_miners.mjs
```

Replace `VERSION` with the archive version in `dist/`. Packaging includes the source component and a generated
`manifest.toml` with an external `.sha256sum`; `plugin.toml` retains the stable module identity and paths.

## CI and releases

CI validates the exact artifact against reconciliation-capable Core `development` with the shared plugin-validation workflow.
The Core setup action checks the binary against `SHA256SUMS`; an extra offline smoke covers miners sharing
one bot account, a second bot account and a user file credential. It also checks identical-destination miner isolation,
edits, retirement, unchanged-revision restoration, retained history and failure recovery. Conformance uses Core's
fixed synthetic selected credential; no live Slack workspace is needed.

Native Rust tests upload LCOV coverage and JUnit results to Codecov via GitHub OIDC.
The repository's `.github/codecov.yml` defines the project, patch and source-module checks.

gh-ship and git-cliff open a reviewed `release/next` PR for releasable commits. After it merges,
the validated `slack-VERSION.tar.gz` archive is published under the `VERSION` tag.
Configure `APP_CLIENT_ID` and
`APP_PRIVATE_KEY` in the `release` environment for an installed GitHub App.
The publish workflow uses this credential to check the draft release and upload its validated archive.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and [AGENTS.md](AGENTS.md) for agent guidance.
