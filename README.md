# memcastle-slack

[![CI](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml) [![Codecov](https://codecov.io/gh/memcastle/memcastle-slack/graph/badge.svg)](https://codecov.io/gh/memcastle/memcastle-slack)

Standalone MemCastle plugin for explicitly scoped Slack conversation and thread history. Supports independently selected bot and user Web API tokens from named source configurations; credentials stay outside plugin configuration and memory. Requires a Core build with [PR #335](https://github.com/memcastle/memcastle/pull/335). See [the source guide](sources/slack/README.md) for Slack app scopes, installation, multiple accounts and independently routed miners.

The plugin contains one `slack` source module. Installation does not enable the source or start a miner; enable it explicitly with `memcastle source enable slack`. It mines only configured channel, private-channel, DM or group-DM IDs, with a stable document per thread and Core-managed revisions, cursors and destinations. Deletions are not propagated by Core.

## Build and validate

Install Rust's `wasm32-wasip2` target and a plugin-capable MemCastle CLI. From the repository root:

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

The archive in `dist/` includes the source component and a generated manifest; `plugin.toml` describes stable module IDs and paths, and `sources/slack/Cargo.lock` locks source dependencies.
CI builds the exact artifact and validates it against Core's `development` prerelease through the shared plugin-validation workflow.
Its extra smoke test covers two miners sharing one bot account, a second bot account and a user file credential.
No live Slack workspace is needed for the offline fixtures.
Set `MEMCASTLE_BIN=/path/to/current/memcastle` when the default CLI predates the source-credential contract.

Native Rust tests upload LCOV coverage and JUnit results to Codecov using GitHub OIDC.
The repository's `.github/codecov.yml` defines project, patch and source-module coverage checks.

Releases use gh-ship and git-cliff: a releasable Conventional Commit on `main` opens a `release/next` PR.
Merging that PR publishes the validated `slack/vVERSION` archive.
The `release` environment supplies the GitHub App variable `APP_CLIENT_ID` and secret `APP_PRIVATE_KEY` to these workflows.
