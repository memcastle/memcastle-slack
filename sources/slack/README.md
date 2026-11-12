# Slack history source

This source mines only explicitly named Slack conversation IDs (public/private channels, DMs, group DMs) through Slack's official Web API. Each root message and its replies form one transcript document; Core files ordered message segments as drawers, retaining Slack IDs, message timestamps and the raw source record. Attachments, files, reactions and removed message contents are not imported. A thread's stable external ID is `CONVERSATION_ID:root_ts` within its workspace/account source identity.

## Prepare Slack access

Create a Slack app with read-only Web API access and install it in the workspace. For the conversation types used, grant the matching `channels:read` + `channels:history` (public), `groups:read` + `groups:history` (private), `im:read` + `im:history` (DM) and/or `mpim:read` + `mpim:history` (group DM) scopes. Slack's bot token sees conversations the bot has joined; a user token sees the user's accessible conversations. Neither token grants blanket access to every private channel or DM. Collect workspace (`T...`), conversation (`C...`, `G...`, `D...`) and optional user (`U...`, `W...`) IDs from Slack. A channel name is not a selector: IDs remain stable across renames.

Use development Core containing [`c311d09`](https://github.com/memcastle/memcastle/commit/c311d09) (Core #378)
or a verified descendant. The source requires reconciliation extension `0.1.0` alongside base contract `0.4.0`.
Older stable builds also labeled `0.5.2` are insufficient; the manifest fails closed on unsupported hosts.
Each `[[source_configs]]` chooses its own `env` variable or `file` reference; `auth_mode` declares whether that reference contains a bot or user token.
Set environment variables **in the daemon's environment**, not only in the shell running the CLI; Core reads the first line of a credential file at execution time.
Never place token values in TOML, options, process arguments or palace drawers.
Only the token selected for a job is passed to the source; `curl` receives it on stdin in an Authorization header and does not inherit the credential variable or file.
Multiple named bot/user accounts and workspaces can coexist, while multiple miners may reuse one account configuration.

Install and enable the plugin, then define one named configuration and two independently scoped miners:

```sh
memcastle plugin install /path/to/dist/slack-VERSION.tar.gz
memcastle source enable slack
```

```toml
[[source_configs]]
id = "slack-work"
source = "slack"
credential = { type = "env", name = "SLACK_WORK_BOT_TOKEN" }
[source_configs.options]
workspace = "T123456789"
account = "work-bot"
auth_mode = "bot"

[[miners]]
name = "slack-project"
source = "slack"
source_config = "slack-work"
wing = "work"
room = "project-discussions"
max_documents = 250
[miners.options]
conversations = ["C123456789"]

[[miners]]
name = "slack-ops"
source = "slack"
source_config = "slack-work"
wing = "operations"
room = "incidents"
mapping = { room_from = "kind" }  # optional: channel / private-channel / dm / group-dm
[miners.options]
conversations = ["G123456789", "D123456789"]
[miners.schedule]
every = "1d"
enabled = false

[[source_configs]]
id = "slack-personal"
source = "slack"
credential = { type = "file", path = "/run/secrets/slack-personal" }
[source_configs.options]
workspace = "T987654321"
account = "personal-user"
auth_mode = "user"

[[miners]]
name = "slack-personal-dm"
source = "slack"
source_config = "slack-personal"
wing = "personal"
[miners.options]
conversations = ["D987654321"]
```

Run `memcastle miner run slack-project`, `memcastle miner run slack-ops` and `memcastle miner run slack-personal-dm`; schedules require explicit enabling.
The first run starts at the beginning of each selected conversation unless `since` is set.
Core gives named miners independent inventory and cursors even with identical account, workspace, scope and destination.
Keep miner names stable through filter changes; renaming a miner creates a new identity and retains the old history.
Mutable conversation/thread/user/date selectors do not change provider identity. Account label, auth mode and workspace identify the sign-in.
Configure another bot account with its own named source config and credential reference the same way; credential references may be rotated without storing their contents in a job.

## Options and behavior

| Option | Meaning |
|---|---|
| `workspace` | Exact `T...` workspace ID, supplied in a source config or as locator; validated with `auth.test`. |
| `account` | Required non-secret slug identifying this sign-in. |
| `auth_mode` | Required `bot` or `user`; the named config's credential provides the token. |
| `conversations` | Required, 1–32 exact `C/G/D...` IDs; no wildcard or implicit workspace-wide scan. |
| `threads` | Optional `CONVERSATION_ID:root_ts` selections within the conversations above. |
| `users` | Optional exact author IDs; retains only their messages within a selected thread. |
| `since` | Optional bootstrap/discovery lower bound, `YYYY-MM-DD` or Slack `seconds.microseconds`. Changes restart discovery; known older threads remain eligible for refresh. |
| `until` | Optional ongoing inclusive upper bound, in the same format; excludes newer roots and trims newer replies during reconciliation. |
| `revisit_days` | 1–30 days, default 7; root-age lookback for incremental discovery, independent of known-thread rechecks. |

### Discovery and reconciliation

Discovery reads newest-first history pages of at most 15 messages, using durable timestamp boundaries rather than long-lived Slack cursors. Per-thread and filtered-only page checkpoints are committed by Core after successful processing. Scan bounds remain fixed across interruptions, and completed high-water marks advance only after a conversation finishes. Completed scans poll no more than once per minute. A document limit bounds processed documents/inventory checks, not the number of filtered history pages.

Before discovery, Core rechecks its persistent inventory, including pending first acquisitions and retired IDs, on normal and `--full` runs. Known old threads therefore receive edits and late replies independently of the discovery lookback. A complete thread read replaces its document after an edit or deleted reply; deleting one reply does not retire the whole thread. Acquisition refuses malformed, repeated or incomplete pagination and oversized threads rather than publishing a partial transcript.

| Evidence | Outcome |
|---|---|
| Conversation/thread excluded by selectors, root beyond `until`, or complete thread with no selected messages | Retire that miner's document; retain history. |
| Explicit root deletion marker or validated `not_in_channel` object-specific access loss | Retire; restoration is possible if it becomes live/accessible again. |
| Empty/missing thread, `thread_not_found`, `channel_not_found`, or retention-limited visibility | Preserve existing knowledge; absence is ambiguous. |
| Archived conversation | Refresh if readable; archive status alone never retires. |
| Invalid/revoked credential, wrong workspace, missing scopes, HTTP permission errors, rate limits, transport or partial-pagination failure | Fail without advancing past the failed acquisition or purging retained knowledge. |

History-listing absence never retires a known ID. Slack retention/free-plan limits can hide content without proving deletion; an explicitly retention-limited thread response is skipped, not ingested as a truncated replacement. The source cannot confirm deletions from ambiguous absence alone. Restoring scope or access refreshes retired inventory even when the provider revision is unchanged. Rate-limit errors include a safe `Retry-After` delay when supplied; retry after that delay. Slack applies tighter history/replies limits to some commercially distributed apps than to internal or Marketplace apps.

### Transition from legacy identities

Earlier versions included mutable selectors in the provider identity and did not opt into Core's named-miner reconciliation partitioning. This upgrade starts new reconciled identities. Existing histories and legacy cursors are deliberately retained, not reassigned or deleted; new runs may initially duplicate legacy retained content. The new inventory cannot clean up those old identities automatically. Keep the old history deliberately and account for this transition when querying historical content. A cursor from an incompatible format requires `--full`.

Only message text is searchable; author/time/thread identifiers and conversation type remain in source metadata. The `kind` metadata field can route messages through Core's generic mapping; otherwise use a fixed Wing/Room/Drawer destination. Do not add a secret-looking key to `[miners.options]`.

## Development and troubleshooting

Build with Rust's `wasm32-wasip2` target, then use the checked-in stand-in `curl` to exercise offline conformance. `[test] fixture_credential = true` selects Core's fixed `memcastle-source-test-credential`; missing credentials are rejected and no live token is needed:

```sh
cargo test --manifest-path sources/slack/Cargo.toml --locked
cargo build --manifest-path sources/slack/Cargo.toml --locked --release --target wasm32-wasip2
mkdir -p sources/slack/dist
cp sources/slack/target/wasm32-wasip2/release/slack.wasm sources/slack/dist/source.wasm
cp sources/slack/dist/source.wasm sources/slack/source.wasm
PATH="$PWD/sources/slack/fixtures/bin:$PATH" "${MEMCASTLE_BIN:-memcastle}" source test sources/slack --no-build
"${MEMCASTLE_BIN:-memcastle}" plugin package
"${MEMCASTLE_BIN:-memcastle}" plugin test dist/slack-VERSION.tar.gz
node scripts/smoke_miners.mjs  # requires Node.js and a plugin-capable Core CLI
```

For live failures, check that the selected source config's env/file reference resolves and matches `auth_mode`.
An incorrect workspace ID fails `auth.test`; correct scopes or membership before rerunning after permission errors.
The source never puts Slack's response body or an auth header in an error.
`revisit_days` controls root-age lookback, not Slack retention or a guarantee to capture deletions.
