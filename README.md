# slack

[![CI](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml)

Scoped Slack conversation history source.

This provider plugin owns the versioned package, while each module has its own lifecycle.
Installing the plugin makes its modules available; enable sources and install integrations separately.
Sources live in `sources/<id>/` and integrations in `integrations/<id>/`; typed modules may share an ID without
sharing their implementation files or activation state.

```sh
memcastle plugin inspect --json
# Build each source for wasm32-wasip2 and each integration with Bun before packaging.
memcastle plugin package
memcastle plugin test dist/ID-VERSION.tar.gz
```

The archive in `dist/` is independently releasable from MemCastle Core.
The root `plugin.toml` is an immutable descriptor of module identities and paths and is not included in the archive.
Core's `plugin package` generates `manifest.toml` **inside the archive only**, with release/module versions and
artifact checksums, and writes a matching `.sha256sum` beside the archive. Commit each source's `Cargo.lock`.
An exact `slack/vVERSION` tag gives the release version; untagged commits receive an identifiable
SemVer development version derived from Git. Use `--version` when packaging without a Git checkout.
The source module carries the versioned public WIT contract and its own fixtures; the agent integration uses MCP/HTTP.
No module needs a private Core checkout or a sibling provider to build or test.
Core's plugin smoke test starts an isolated daemon and checks archive installation and source activation;
it does not register the integration with an agent.
Provider `ci.yaml` builds, tests and packages locally, then calls Core's shared artifact-validation workflow for the
static Core matrix selected by `core_versions` (default `main`). When plugin-capable stable releases exist,
selecting a minor tree such as `0.5` tests its latest published patch; `main` uses the development prerelease.
The reusable validation workflow follows Core's `main` branch without a revision pin.
It runs on every pull request and every push to `main`, including merges, without a second push run on PR branches.
Every version exercises source fixtures and archive installation/activation; incompatible versions fail the workflow.
The development release body records the exact Core source SHA, and the workflow summary reports it for each run.
Core CI separately tests a freshly composed project against its current install contract.

The provider's own release workflow accepts only a `slack/vVERSION` tag on `main`, then builds
the reviewed artifact, runs manifest and compatibility-matrix tests, and refuses an existing release.

Keep IDs stable when moving this project from `plugins/slack/` to its own repository.


