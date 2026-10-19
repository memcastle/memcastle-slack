# Slack history source

This source mines only explicitly named Slack conversation IDs (public/private channels, DMs, group DMs) through Slack's official Web API. Each root message and its replies form one transcript document; Core files ordered message segments as drawers, retaining Slack IDs, message timestamps and the raw source record. Attachments, files, reactions and removed message contents are not imported. A thread's stable external ID is `CONVERSATION_ID:root_ts` within its workspace/account source identity.

## Prepare Slack access

Create a Slack app with read-only Web API access and install it in the workspace. For the conversation types used, grant the matching `channels:read` + `channels:history` (public), `groups:read` + `groups:history` (private), `im:read` + `im:history` (DM) and/or `mpim:read` + `mpim:history` (group DM) scopes. Slack's bot token sees conversations the bot has joined; a user token sees the user's accessible conversations. Neither token grants blanket access to every private channel or DM. Collect workspace (`T...`), conversation (`C...`, `G...`, `D...`) and optional user (`U...`, `W...`) IDs from Slack. A channel name is not a selector: IDs remain stable across renames.

Use a Core build including [PR #335](https://github.com/memcastle/memcastle/pull/335) for selected source-config credentials.
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
The shared sign-in/configuration is not the cursor: scope, named configuration, account and Core destination mapping give each miner an independent source identity.
Renaming a miner without changing scope/mapping retains its cursor.
A selected conversation that is inaccessible (including private channels a bot did not join), archived or deleted fails the run instead of being silently skipped.
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
| `since` / `until` | Optional `YYYY-MM-DD` or Slack `seconds.microseconds` lower/upper history bounds. Moving `since` backward needs `--full`. |
| `revisit_days` | 1–30 days, default 7; revisits recent roots for edits/replies during later scans. |

The source scans in bounded Slack pages (15 messages/request) and checkpoints each thread through Core. It refuses incomplete pagination, oversized histories and malformed records instead of treating a partial reply set as a complete document. It uses an API-time window based on the latest processed root in each conversation and polls at most once per minute for a completed scan. Older edited roots or late replies to older threads need `memcastle miner run NAME --full` or an explicit `threads` selection. Slack history's timestamp pagination does **not** provide an edit stream. Core currently does not propagate deletions of documents that vanished from Slack; removing or losing access to a conversation does not erase previously filed drawers. Rate-limit errors include a safe `Retry-After` delay when supplied; retry the job after that delay. Slack applies much tighter `conversations.history` and `conversations.replies` limits to some commercially distributed apps than to internal or Marketplace apps.

Only message text is searchable; author/time/thread identifiers and conversation type remain in source metadata. The `kind` metadata field can route messages through Core's generic mapping; otherwise use a fixed Wing/Room/Drawer destination. Do not add a secret-looking key to `[miners.options]`.

## Development and troubleshooting

Build with Rust's `wasm32-wasip2` target, then use the checked-in stand-in `curl` to exercise offline conformance (the fixture intentionally responds without a real Slack credential):

```sh
cargo test --manifest-path sources/slack/Cargo.toml --locked
cargo build --manifest-path sources/slack/Cargo.toml --locked --release --target wasm32-wasip2
mkdir -p sources/slack/dist
cp sources/slack/target/wasm32-wasip2/release/slack.wasm sources/slack/dist/source.wasm
PATH="$PWD/sources/slack/fixtures/bin:$PATH" memcastle source test sources/slack --no-build
memcastle plugin package
memcastle plugin test dist/slack-VERSION.tar.gz
node scripts/smoke_miners.mjs  # requires Node.js and a plugin-capable Core CLI
```

For live failures, check that the selected source config's env/file reference resolves and matches `auth_mode`.
An incorrect workspace ID fails `auth.test`; `missing_scope`, `not_in_channel` and inaccessible/archived conversations require correcting the Slack app's scopes or membership and then rerunning.
The source never puts Slack's response body or an auth header in an error.
`revisit_days` controls root-age lookback, not Slack retention or a guarantee to capture deletions.
