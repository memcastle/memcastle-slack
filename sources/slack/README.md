# Slack history source

This source mines only explicitly named Slack conversation IDs (public/private channels, DMs, group DMs) through Slack's official Web API. Each root message and its replies form one transcript document; Core files ordered message segments as drawers, retaining Slack IDs, message timestamps and the raw source record. Attachments, files, reactions and removed message contents are not imported. A thread's stable external ID is `CONVERSATION_ID:root_ts` within its workspace/account source identity.

## Prepare Slack access

Create a Slack app with read-only Web API access and install it in the workspace. For the conversation types used, grant the matching `channels:read` + `channels:history` (public), `groups:read` + `groups:history` (private), `im:read` + `im:history` (DM) and/or `mpim:read` + `mpim:history` (group DM) scopes. Slack's bot token sees conversations the bot has joined; a user token sees the user's accessible conversations. Neither token grants blanket access to every private channel or DM. Collect workspace (`T...`), conversation (`C...`, `G...`, `D...`) and optional user (`U...`, `W...`) IDs from Slack. A channel name is not a selector: IDs remain stable across renames.

Until [Core's named-source credential grant](https://github.com/memcastle/memcastle/issues/332) is available, set `MEMCASTLE_SLACK_BOT_TOKEN` and/or `MEMCASTLE_SLACK_USER_TOKEN` **in the daemon's environment**. Never place a token in TOML, an option, a process argument or a palace drawer. Source configs can share the bot or user sign-in, but separate simultaneously active bot accounts (or separate user accounts) need the Core change before their credentials can be selected independently. Set `auth_mode` to select the matching temporary variable; no token is inherited from the shell running only the CLI if the daemon has a different environment.

Install and enable the plugin, then define one named configuration and two independently scoped miners:

```sh
memcastle plugin install /path/to/dist/slack-VERSION.tar.gz
memcastle source enable slack
```

```toml
[[source_configs]]
id = "slack-work"
source = "slack"
credential = { type = "env", name = "MEMCASTLE_SLACK_BOT_TOKEN" }
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
```

Run `memcastle miner run slack-project` and `memcastle miner run slack-ops`; schedules require explicit enabling. The first run starts at the beginning of each selected conversation unless `since` is set. The shared sign-in/configuration is not the cursor: scope, account and Core destination mapping give each miner its own source identity. Renaming a miner without changing scope/mapping retains its cursor. A selected conversation that is inaccessible (including private channels a bot did not join), archived or deleted fails the run instead of being silently skipped. A user-mode source config can use `credential = { type = "env", name = "MEMCASTLE_SLACK_USER_TOKEN" }` and `auth_mode = "user"`.

## Options and behavior

| Option | Meaning |
|---|---|
| `workspace` | Exact `T...` workspace ID, supplied in a source config or as locator; validated with `auth.test`. |
| `account` | Required non-secret slug identifying this sign-in. |
| `auth_mode` | Required `bot` or `user`. |
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

For live failures, check the daemon's variable and selected `auth_mode`; an incorrect workspace ID fails `auth.test`; `missing_scope`, `not_in_channel` and inaccessible/archived conversations require correcting the Slack app's scopes or membership and then rerunning. The source never puts Slack's response body or an auth header in an error. `revisit_days` controls root-age lookback, not Slack retention or a guarantee to capture deletions.
