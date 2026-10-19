# slack

[![CI](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml/badge.svg?branch=main)](https://github.com/memcastle/memcastle-slack/actions/workflows/ci.yaml) [![Codecov](https://codecov.io/gh/memcastle/memcastle-slack/graph/badge.svg)](https://codecov.io/gh/memcastle/memcastle-slack)

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
static Core matrix selected by `core_versions` (default `development`). When plugin-capable stable releases exist,
selecting a minor tree such as `0.5` tests its latest published patch; `development` uses the moving prerelease.
The reusable validation workflow follows Core's `main` branch without a revision pin.
It runs on every pull request and every push to `main`, including merges, without a second push run on PR branches.
Every version exercises source fixtures and archive installation/activation; incompatible versions fail the workflow.
The development release body records the exact Core source SHA, and the workflow summary reports it for each run.
Core CI separately tests a freshly composed project against its current install contract.

The publish workflow validates the reviewed `slack/vVERSION` tag on `main`, rebuilds the
archive and runs the compatibility matrix before uploading to gh-ship's draft release.

Keep IDs stable when moving this project from `plugins/slack/` to its own repository.

The source/integration jobs in `ci.yaml` measure native Rust and Bun tests and upload LCOV and JUnit reports
to Codecov using OIDC. Enable this repository in Codecov to see PR coverage and test results.
Manifest-only suites produce test results but no fabricated coverage report.
The Codecov project and patch checks have fixed targets (`80%` and
`80%`); coverage-producing source and integration components use
`80%` and `80%` respectively.
Only IDs selected in `codecov_covered_sources` or `codecov_covered_integrations` block on component coverage;
modules without nonempty LCOV remain informational. Update those lists when adding native tests.


Releases use gh-ship and git-cliff: merging a releasable Conventional Commit into `main` opens a `release/next` PR.
Merging that PR publishes a validated `slack/vVERSION` archive as a GitHub Release.
Set up the `release` environment with variable `APP_CLIENT_ID` and secret `APP_PRIVATE_KEY`
for an App installed on this repository.
Grant contents, actions and pull requests write permission; issues write is needed to create the release label.
Set squash merge title to PR title and body to blank, and enable this repository's release environment.
The `prepare-release` workflow honors its `dry_run` input without pushing or committing.

