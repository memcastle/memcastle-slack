//! Explicitly scoped Slack history. Core owns jobs, chunking and durable cursor writes.

mod api;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

wit_bindgen::generate!({ path: "wit", world: "source" });

use exports::memcastle::source::adapter::Guest;
use memcastle::source::types::{
    Candidate, CanonicalDocument, Discovery, RawDocument, Segment, SourceError, SourceKind,
    SourceRef,
};

const MAX_CONVERSATIONS: usize = 32;
const MAX_MESSAGES: usize = 30_000;
const MAX_THREAD_MESSAGES: usize = 2_000;
const DEFAULT_REVISIT_DAYS: u64 = 7;

struct Slack;

fn option<'a>(source: &'a SourceRef, name: &str) -> Option<&'a str> {
    source
        .options
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn list(value: &str) -> Result<Vec<String>, String> {
    let mut entries = BTreeSet::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() || entry.chars().any(char::is_control) {
            return Err("scope lists require nonempty comma-separated entries".into());
        }
        entries.insert(entry.to_string());
    }
    Ok(entries.into_iter().collect())
}

fn slack_id(id: &str, prefixes: &str) -> bool {
    id.len() >= 9
        && id.len() <= 32
        && id.starts_with(|c: char| prefixes.contains(c))
        && id[1..].bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn timestamp(value: &str) -> Result<u64, String> {
    let (sec, micros) = value
        .split_once('.')
        .ok_or("Slack timestamp must be seconds.microseconds")?;
    if !(1..=12).contains(&sec.len())
        || micros.len() != 6
        || !sec
            .bytes()
            .chain(micros.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err("Slack timestamp must be seconds.six-digit microseconds".into());
    }
    let seconds = sec.parse::<u64>().map_err(|_| "invalid timestamp")?;
    if seconds == 0 {
        return Err("Slack timestamp cannot be zero".into());
    }
    seconds
        .checked_mul(1_000_000)
        .and_then(|base| base.checked_add(micros.parse::<u64>().ok()?))
        .ok_or_else(|| "Slack timestamp is out of range".into())
}

fn slack_time(value: &str) -> Result<String, String> {
    let micros = timestamp(value)?;
    let seconds = i64::try_from(micros / 1_000_000).map_err(|_| "invalid timestamp")?;
    DateTime::<Utc>::from_timestamp(seconds, (micros % 1_000_000) as u32 * 1_000)
        .map(|date| date.to_rfc3339())
        .ok_or_else(|| "invalid timestamp".into())
}

fn date(value: &str) -> Result<u64, String> {
    if let Ok(ts) = timestamp(value) {
        return Ok(ts);
    }
    let day = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "date must be YYYY-MM-DD or a Slack timestamp")?;
    u64::try_from(day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
        .map(|seconds| seconds * 1_000_000)
        .map_err(|_| "date must be 1970-01-01 or later".into())
}

fn thread_ref(value: &str) -> Result<(String, String), String> {
    let (conversation, root) = value
        .split_once(':')
        .ok_or("threads entries must be CONVERSATION_ID:root_ts")?;
    if !slack_id(conversation, "CGD") || timestamp(root).is_err() {
        return Err("threads entries must be CONVERSATION_ID:root_ts".into());
    }
    Ok((conversation.into(), root.into()))
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    /// Latest root observed on a completed or in-progress scan (per conversation).
    high: BTreeMap<String, u64>,
    /// Unix second of the scan; a new run only starts on a later second.
    scan_at: u64,
    /// Last candidate of an in-progress scan, in deterministic (conversation, timestamp) order.
    after: Option<(String, u64)>,
    complete: bool,
}

fn cursor(value: &str) -> Result<Option<Cursor>, SourceError> {
    let raw: Value = serde_json::from_str(value)
        .map_err(|_| SourceError::CursorInvalid("Slack cursor is not JSON; use --full".into()))?;
    if raw.is_null() {
        return Ok(None);
    }
    let state: Cursor = serde_json::from_value(raw)
        .map_err(|_| SourceError::CursorInvalid("unknown Slack cursor; use --full".into()))?;
    if state.high.keys().any(|key| !slack_id(key, "CGD"))
        || state
            .after
            .as_ref()
            .is_some_and(|(key, ts)| !slack_id(key, "CGD") || *ts == 0)
        || state.scan_at == 0
    {
        return Err(SourceError::CursorInvalid(
            "invalid Slack cursor; use --full".into(),
        ));
    }
    Ok(Some(state))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handle {
    conversation: String,
    root: String,
    name: String,
    kind: String,
}

fn message_ts(message: &Value) -> Result<(String, u64), String> {
    let ts = message
        .get("ts")
        .and_then(Value::as_str)
        .ok_or("Slack message lacks ts")?;
    Ok((ts.into(), timestamp(ts)?))
}

fn retain(message: &Value) -> bool {
    message.get("type").and_then(Value::as_str) == Some("message")
        && !matches!(
            message.get("subtype").and_then(Value::as_str),
            Some("channel_join" | "channel_leave" | "message_deleted")
        )
        && message.get("text").and_then(Value::as_str).is_some()
}

fn selected_users(source: &SourceRef) -> Vec<String> {
    option(source, "users")
        .map(|value| value.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

fn selected_threads(source: &SourceRef) -> Vec<(String, String)> {
    option(source, "threads")
        .map(|value| {
            value
                .split(',')
                .filter_map(|entry| thread_ref(entry).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn conversations(source: &SourceRef) -> Vec<String> {
    option(source, "conversations")
        .map(|value| value.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

fn conversation_info(source: &SourceRef, channel: &str) -> Result<(String, String), String> {
    let info = api::request(source, "conversations.info", &[("channel", channel)])?;
    let item = info
        .get("channel")
        .ok_or("Slack conversation has no metadata")?;
    if item.get("id").and_then(Value::as_str) != Some(channel)
        || item.get("is_archived").and_then(Value::as_bool) == Some(true)
        || item.get("is_member").and_then(Value::as_bool) == Some(false)
    {
        return Err(format!(
            "conversation {channel} is unavailable, archived or inaccessible"
        ));
    }
    let kind = if item.get("is_im").and_then(Value::as_bool) == Some(true) {
        "dm"
    } else if item.get("is_mpim").and_then(Value::as_bool) == Some(true) {
        "group-dm"
    } else if item.get("is_private").and_then(Value::as_bool) == Some(true) {
        "private-channel"
    } else if item.get("is_channel").and_then(Value::as_bool) == Some(true) {
        "channel"
    } else {
        return Err(format!("conversation {channel} has an unsupported type"));
    };
    Ok((
        item.get("name")
            .and_then(Value::as_str)
            .unwrap_or(channel)
            .into(),
        kind.into(),
    ))
}

fn page_messages(
    source: &SourceRef,
    method: &str,
    params: &[(&str, &str)],
    cap: usize,
) -> Result<Vec<Value>, String> {
    let mut all = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = String::new();
    for _ in 0..2_000 {
        let mut query = params.to_vec();
        query.push(("limit", "15")); // also valid for Slack's newer one-request-per-minute tier
        if !cursor.is_empty() {
            query.push(("cursor", &cursor));
        }
        let response = api::request(source, method, &query)?;
        let messages = response
            .get("messages")
            .and_then(Value::as_array)
            .ok_or("Slack response has no messages array")?;
        if messages.len() > 15 || all.len() + messages.len() > cap {
            return Err(format!(
                "Slack {method} exceeds the safe page/message bound; narrow the scope"
            ));
        }
        all.extend(messages.iter().cloned());
        let next = response
            .pointer("/response_metadata/next_cursor")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if next.is_empty() {
            if response.get("has_more").and_then(Value::as_bool) == Some(true) {
                return Err(format!(
                    "Slack {method} indicated more results without a cursor"
                ));
            }
            return Ok(all);
        }
        if !seen.insert(next.to_string()) {
            return Err(format!("Slack {method} repeated its page cursor"));
        }
        cursor = next.into();
    }
    Err(format!(
        "Slack {method} exceeds 2,000 pages; narrow the scope"
    ))
}

fn roots(
    source: &SourceRef,
    channel: &str,
    oldest: u64,
    until: Option<u64>,
) -> Result<Vec<(String, u64)>, String> {
    let oldest_text = format!("{}.{:06}", oldest / 1_000_000, oldest % 1_000_000);
    let latest_text = until.map(|ts| format!("{}.{:06}", ts / 1_000_000, ts % 1_000_000));
    let mut params = vec![
        ("channel", channel),
        ("oldest", oldest_text.as_str()),
        ("inclusive", "true"),
    ];
    if let Some(latest) = latest_text.as_deref() {
        params.push(("latest", latest));
    }
    let mut roots = Vec::new();
    for message in page_messages(source, "conversations.history", &params, MAX_MESSAGES)? {
        let (ts, number) = message_ts(&message)?;
        if message
            .get("thread_ts")
            .and_then(Value::as_str)
            .is_some_and(|root| root != ts)
        {
            continue;
        }
        if number >= oldest && until.is_none_or(|latest| number <= latest) && retain(&message) {
            roots.push((ts, number));
        }
    }
    roots.sort_by_key(|(_, number)| *number);
    roots.dedup_by_key(|(_, number)| *number);
    Ok(roots)
}

fn thread(source: &SourceRef, channel: &str, root: &str) -> Result<Vec<Value>, String> {
    let messages = page_messages(
        source,
        "conversations.replies",
        &[("channel", channel), ("ts", root)],
        MAX_THREAD_MESSAGES,
    )?;
    if messages.is_empty() || message_ts(&messages[0])?.0 != root {
        return Err(format!(
            "Slack thread {channel}:{root} is absent or incomplete"
        ));
    }
    let mut by_ts = BTreeMap::new();
    for message in messages {
        let (ts, number) = message_ts(&message)?;
        if message
            .get("thread_ts")
            .and_then(Value::as_str)
            .is_some_and(|parent| parent != root)
        {
            return Err(format!(
                "Slack thread {channel}:{root} has a mismatched root"
            ));
        }
        if by_ts.insert(number, message).is_some() {
            return Err(format!("Slack thread {channel}:{root} repeated a message"));
        }
        let _ = ts;
    }
    Ok(by_ts.into_values().collect())
}

fn normalize_document(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
    let meta: Value = serde_json::from_str(&raw.metadata)
        .map_err(|_| SourceError::Failed("invalid Slack document metadata".into()))?;
    let channel = meta
        .get("conversation")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Failed("Slack document lacks conversation".into()))?;
    let root = meta
        .get("root")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Failed("Slack document lacks thread root".into()))?;
    let messages: Vec<Value> = serde_json::from_str(&raw.body)
        .map_err(|_| SourceError::Failed("invalid Slack thread body".into()))?;
    let mut segments = Vec::new();
    for message in messages.iter().filter(|m| retain(m)) {
        let ts = message
            .get("ts")
            .and_then(Value::as_str)
            .ok_or_else(|| SourceError::Failed("Slack message lacks timestamp".into()))?;
        let speaker = message
            .get("user")
            .or_else(|| message.get("bot_id"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let text = message.get("text").and_then(Value::as_str).unwrap_or("");
        segments.push(Segment {
            text: format!("[{ts}] {speaker}: {text}"),
        });
    }
    Ok(CanonicalDocument {
        title: Some(format!(
            "{} · {}",
            meta.get("name").and_then(Value::as_str).unwrap_or(channel),
            root
        )),
        room: Some("conversations".into()),
        name: None,
        kind: SourceKind::Transcript,
        uri: Some(format!(
            "https://app.slack.com/archives/{channel}/p{}",
            root.replace('.', "")
        )),
        tags: vec![
            "slack".into(),
            format!("conversation:{channel}"),
            format!(
                "kind:{}",
                meta.get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("conversation")
            ),
        ],
        segments,
    })
}

impl Guest for Slack {
    fn identify(
        locator: Option<String>,
        options: Vec<(String, String)>,
    ) -> Result<SourceRef, SourceError> {
        let mut values = BTreeMap::new();
        for (key, value) in options {
            if values.contains_key(&key) {
                return Err(SourceError::InvalidInput(format!(
                    "duplicate Slack option {key}"
                )));
            }
            let value = value.trim();
            let normalized = match key.as_str() {
                "workspace" => {
                    if !slack_id(value, "T") {
                        return Err(SourceError::InvalidInput(
                            "workspace must be an exact Slack workspace ID (T...)".into(),
                        ));
                    }
                    value.into()
                }
                "conversations" => {
                    let entries = list(value).map_err(SourceError::InvalidInput)?;
                    if entries.is_empty()
                        || entries.len() > MAX_CONVERSATIONS
                        || entries.iter().any(|id| !slack_id(id, "CGD"))
                    {
                        return Err(SourceError::InvalidInput(
                            "conversations must contain 1–32 exact C/G/D conversation IDs".into(),
                        ));
                    }
                    entries.join(",")
                }
                "threads" => {
                    let entries = list(value).map_err(SourceError::InvalidInput)?;
                    if entries.iter().any(|entry| thread_ref(entry).is_err()) {
                        return Err(SourceError::InvalidInput(
                            "threads must be CONVERSATION_ID:root_ts entries".into(),
                        ));
                    }
                    entries.join(",")
                }
                "users" => {
                    let entries = list(value).map_err(SourceError::InvalidInput)?;
                    if entries.iter().any(|id| !slack_id(id, "UW")) {
                        return Err(SourceError::InvalidInput(
                            "users must contain exact U/W user IDs".into(),
                        ));
                    }
                    entries.join(",")
                }
                "account" => {
                    if value.is_empty()
                        || value.len() > 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                    {
                        return Err(SourceError::InvalidInput(
                            "account must be a non-secret slug".into(),
                        ));
                    }
                    value.into()
                }
                "auth_mode" if matches!(value, "bot" | "user") => value.into(),
                "auth_mode" => {
                    return Err(SourceError::InvalidInput(
                        "auth_mode must be bot or user".into(),
                    ));
                }
                "since" | "until" => {
                    date(value).map_err(SourceError::InvalidInput)?;
                    value.into()
                }
                "revisit_days" => {
                    if !(1..=30).contains(&value.parse::<u64>().unwrap_or(0)) {
                        return Err(SourceError::InvalidInput(
                            "revisit_days must be between 1 and 30".into(),
                        ));
                    }
                    value.into()
                }
                _ => {
                    return Err(SourceError::InvalidInput(format!(
                        "unknown Slack option {key}"
                    )));
                }
            };
            values.insert(key, normalized);
        }
        let workspace = match (locator.as_deref(), values.get("workspace")) {
            (Some(locator), Some(workspace)) if slack_id(locator, "T") && locator != workspace => {
                return Err(SourceError::InvalidInput(
                    "locator and workspace option disagree".into(),
                ));
            }
            (_, Some(workspace)) => workspace.clone(),
            (Some(locator), None) if slack_id(locator, "T") => locator.into(),
            _ => {
                return Err(SourceError::InvalidInput(
                    "set workspace=T... in source options or give a workspace ID as locator".into(),
                ));
            }
        };
        let selected = values.get("conversations").ok_or_else(|| {
            SourceError::InvalidInput("conversations requires at least one explicit ID".into())
        })?;
        if !values.contains_key("account") || !values.contains_key("auth_mode") {
            return Err(SourceError::InvalidInput(
                "account and auth_mode are required source configuration options".into(),
            ));
        }
        if let Some(threads) = values.get("threads") {
            let allowed: BTreeSet<_> = selected.split(',').collect();
            if threads
                .split(',')
                .any(|entry| !allowed.contains(entry.split(':').next().unwrap_or("")))
            {
                return Err(SourceError::InvalidInput(
                    "threads must belong to selected conversations".into(),
                ));
            }
        }
        if let (Some(since), Some(until)) = (values.get("since"), values.get("until"))
            && date(since).unwrap() > date(until).unwrap()
        {
            return Err(SourceError::InvalidInput(
                "since must be before until".into(),
            ));
        }
        // Filter/scope changes create independent cursors; the bootstrap boundary is only narrowing.
        let selectors: Vec<_> = values
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "since" | "revisit_days" | "workspace"))
            .collect();
        let account = format!(
            "{}:{}",
            values["account"],
            digest(&serde_json::to_string(&selectors).unwrap())
        );
        Ok(SourceRef {
            source: "slack".into(),
            account: Some(account),
            locator: workspace,
            options: values.into_iter().collect(),
        })
    }

    fn default_wing(_source: SourceRef) -> String {
        "slack".into()
    }
    fn default_room() -> String {
        "conversations".into()
    }

    fn discover(source: SourceRef, position: String, limit: u32) -> Result<Discovery, SourceError> {
        let auth = api::request(&source, "auth.test", &[]).map_err(SourceError::Failed)?;
        if auth.get("team_id").and_then(Value::as_str) != Some(source.locator.as_str()) {
            return Err(SourceError::Failed(
                "Slack token belongs to a different workspace than the locator".into(),
            ));
        }
        let previous = cursor(&position)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SourceError::Failed("invalid system clock".into()))?
            .as_secs();
        // A completed scan must stay exhausted long enough for the Core pipeline and
        // conformance runner to observe it, even if the wall clock crosses a second.
        if previous
            .as_ref()
            .is_some_and(|state| state.complete && now < state.scan_at.saturating_add(60))
        {
            return Ok(Discovery {
                candidates: vec![],
                exhausted: true,
            });
        }
        let new_scan = previous.as_ref().is_none_or(|state| state.complete);
        let mut state = previous.unwrap_or(Cursor {
            high: BTreeMap::new(),
            scan_at: now,
            after: None,
            complete: false,
        });
        if new_scan {
            state.scan_at = now;
            state.after = None;
            state.complete = false;
        }
        let since = option(&source, "since")
            .map(date)
            .transpose()
            .map_err(SourceError::InvalidInput)?
            .unwrap_or(0);
        let until = option(&source, "until")
            .map(date)
            .transpose()
            .map_err(SourceError::InvalidInput)?;
        let window = option(&source, "revisit_days")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_REVISIT_DAYS)
            * 86_400_000_000;
        let explicit = selected_threads(&source);
        let mut found = Vec::new();
        for channel in conversations(&source) {
            let (name, kind) = conversation_info(&source, &channel).map_err(SourceError::Failed)?;
            let oldest = state
                .high
                .get(&channel)
                .copied()
                .map_or(since, |high| high.saturating_sub(window).max(since));
            // Exact thread selectors do not need a conversation-wide history listing.
            let mut roots = if option(&source, "threads").is_some() {
                Vec::new()
            } else {
                roots(&source, &channel, oldest, until).map_err(SourceError::Failed)?
            };
            roots.extend(
                explicit
                    .iter()
                    .filter(|(id, _)| id == &channel)
                    .filter_map(|(_, root)| timestamp(root).ok().map(|ts| (root.clone(), ts)))
                    .filter(|(_, ts)| *ts >= since && until.is_none_or(|end| *ts <= end)),
            );
            roots.sort_by_key(|(_, number)| *number);
            roots.dedup_by_key(|(_, number)| *number);
            for (root, number) in roots {
                if state
                    .after
                    .as_ref()
                    .is_some_and(|after| (&channel, number) <= (&after.0, after.1))
                {
                    continue;
                }
                if let Some(filter) = option(&source, "threads")
                    && !filter
                        .split(',')
                        .any(|entry| entry == format!("{channel}:{root}"))
                {
                    continue;
                }
                found.push((channel.clone(), name.clone(), kind.clone(), root, number));
            }
        }
        found.sort_by(|left, right| (&left.0, left.4).cmp(&(&right.0, right.4)));
        let len = found.len();
        let max = limit as usize;
        let mut candidates = Vec::new();
        for (idx, (channel, name, kind, root, number)) in found.into_iter().take(max).enumerate() {
            state.after = Some((channel.clone(), number));
            state
                .high
                .entry(channel.clone())
                .and_modify(|high| *high = (*high).max(number))
                .or_insert(number);
            state.complete = idx + 1 == len;
            let handle = Handle {
                conversation: channel.clone(),
                root: root.clone(),
                name,
                kind,
            };
            candidates.push(Candidate {
                external_id: format!("{channel}:{root}"),
                cursor_after: serde_json::to_string(&state).unwrap(),
                handle: serde_json::to_string(&handle).unwrap(),
            });
        }
        Ok(Discovery {
            candidates,
            exhausted: len <= max,
        })
    }

    fn read(source: SourceRef, candidate: Candidate) -> Result<Option<RawDocument>, SourceError> {
        let handle: Handle = serde_json::from_str(&candidate.handle)
            .map_err(|_| SourceError::Failed("invalid Slack candidate".into()))?;
        if !conversations(&source).contains(&handle.conversation)
            || candidate.external_id != format!("{}:{}", handle.conversation, handle.root)
        {
            return Err(SourceError::Failed(
                "Slack candidate falls outside selected conversation".into(),
            ));
        }
        let all =
            thread(&source, &handle.conversation, &handle.root).map_err(SourceError::Failed)?;
        let users = selected_users(&source);
        let until = option(&source, "until")
            .map(date)
            .transpose()
            .map_err(SourceError::InvalidInput)?;
        let retained: Vec<_> = all
            .into_iter()
            .filter(|message| {
                retain(message)
                    && until.is_none_or(|end| message_ts(message).is_ok_and(|(_, ts)| ts <= end))
                    && (users.is_empty()
                        || message
                            .get("user")
                            .and_then(Value::as_str)
                            .is_some_and(|user| users.iter().any(|selected| selected == user)))
            })
            .collect();
        if retained.is_empty() {
            return Ok(None);
        }
        let occurred_at = slack_time(&handle.root).map_err(SourceError::Failed)?;
        // Retain only source-native fields needed for transcript/provenance; never embed auth or arbitrary Slack payloads.
        let messages: Vec<_> = retained.iter().map(|m| json!({"type":"message","ts":m["ts"],"thread_ts":m.get("thread_ts"),"user":m.get("user"),"bot_id":m.get("bot_id"),"text":m["text"],"edited":m.get("edited")})).collect();
        let body = serde_json::to_string(&messages).unwrap();
        let metadata = json!({"workspace": source.locator, "conversation":handle.conversation,"root":handle.root,"name":handle.name,"kind":handle.kind,"message_count":messages.len(),"occurred_at":occurred_at});
        Ok(Some(RawDocument {
            external_id: candidate.external_id,
            revision: digest(&format!("{body}\0{metadata}")),
            body,
            metadata: metadata.to_string(),
            occurred_at: Some(occurred_at),
        }))
    }

    fn normalize(raw: RawDocument) -> Result<CanonicalDocument, SourceError> {
        normalize_document(raw)
    }
}

export!(Slack);

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn source(extra: &[(&str, &str)]) -> SourceRef {
        Slack::identify(
            Some("T123456789".into()),
            [
                ("account", "work"),
                ("auth_mode", "bot"),
                ("conversations", "C123456789"),
            ]
            .into_iter()
            .filter(|(key, _)| !extra.iter().any(|(replace, _)| key == replace))
            .chain(extra.iter().copied())
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        )
        .unwrap()
    }

    #[test]
    fn explicit_scope_and_stable_identity() {
        assert!(Slack::identify(Some("T123456789".into()), vec![]).is_err());
        for (key, value) in [
            ("conversations", "*"),
            ("conversations", "C123456789,"),
            ("users", "all"),
            ("threads", "C123456789:bogus"),
            ("auth_mode", "admin"),
            ("unknown", "value"),
        ] {
            let options = [
                ("account", "work"),
                ("auth_mode", "bot"),
                ("conversations", "C123456789"),
            ]
            .into_iter()
            .filter(|(k, _)| k != &key)
            .chain([(key, value)])
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
            assert!(
                Slack::identify(Some("T123456789".into()), options).is_err(),
                "{key}={value}"
            );
        }
        assert_eq!(
            source(&[]).account,
            source(&[("since", "2026-01-01")]).account
        );
        assert_eq!(
            source(&[]).account,
            source(&[("workspace", "T123456789")]).account
        );
        assert_ne!(
            source(&[]).account,
            source(&[("users", "U123456789")]).account
        );
        assert_ne!(source(&[]).account, source(&[("account", "other")]).account);
        assert_ne!(
            source(&[]).account,
            source(&[("auth_mode", "user")]).account
        );
        assert_eq!(
            source(&[("conversations", "D123456789,C123456789")]).account,
            source(&[("conversations", "C123456789,D123456789")]).account
        );
    }

    #[test]
    fn cursor_and_thread_normalization() {
        assert!(cursor("null").unwrap().is_none());
        assert!(cursor("{}").is_err());
        let raw = RawDocument { external_id:"C123456789:1760000000.000001".into(),revision:"x".into(),body:json!([{"type":"message","ts":"1760000000.000001","user":"U123456789","text":"hello"},{"type":"message","ts":"1760000001.000001","user":"U987654321","text":"reply"}]).to_string(),metadata:json!({"conversation":"C123456789","root":"1760000000.000001","name":"general","kind":"channel"}).to_string(),occurred_at:None };
        let doc = Slack::normalize(raw).unwrap();
        assert_eq!(doc.kind, SourceKind::Transcript);
        assert_eq!(doc.segments.len(), 2);
        assert!(doc.segments[1].text.contains("reply"));
        assert!(doc.uri.unwrap().contains("C123456789"));
    }

    #[test]
    fn paged_discovery_and_repeated_reads_are_stable() {
        let source = source(&[]);
        let first = Slack::discover(source.clone(), "null".into(), 1).unwrap();
        assert_eq!(first.candidates.len(), 1);
        assert!(!first.exhausted);
        let second =
            Slack::discover(source.clone(), first.candidates[0].cursor_after.clone(), 1).unwrap();
        assert_eq!(second.candidates.len(), 1);
        assert!(second.exhausted);
        assert_ne!(
            first.candidates[0].external_id,
            second.candidates[0].external_id
        );
        let after =
            Slack::discover(source.clone(), second.candidates[0].cursor_after.clone(), 1).unwrap();
        assert!(after.candidates.is_empty());
        assert!(after.exhausted);
        let mut later: Cursor = serde_json::from_str(&second.candidates[0].cursor_after).unwrap();
        later.scan_at -= 61;
        let revisit =
            Slack::discover(source.clone(), serde_json::to_string(&later).unwrap(), 1).unwrap();
        assert_eq!(
            revisit.candidates[0].external_id,
            first.candidates[0].external_id
        );
        let raw = Slack::read(source.clone(), first.candidates[0].clone())
            .unwrap()
            .unwrap();
        let again = Slack::read(source, first.candidates[0].clone())
            .unwrap()
            .unwrap();
        assert_eq!(raw.revision, again.revision);
        assert_eq!(Slack::normalize(raw).unwrap().segments.len(), 2);
    }

    #[test]
    fn independent_miners_and_user_filters_do_not_share_cursors() {
        let project = source(&[("conversations", "C123456789")]);
        let direct = source(&[("conversations", "D123456789")]);
        let second_account = source(&[("account", "other-bot")]);
        assert_ne!(project.account, direct.account);
        assert_ne!(project.account, second_account.account);
        let candidates = Slack::discover(direct.clone(), "null".into(), 2)
            .unwrap()
            .candidates;
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|c| c.external_id.starts_with('D')));
        let only_user = source(&[("users", "U987654321")]);
        let first = Slack::discover(only_user.clone(), "null".into(), 1)
            .unwrap()
            .candidates
            .remove(0);
        let raw = Slack::read(only_user, first).unwrap().unwrap();
        assert_eq!(Slack::normalize(raw).unwrap().segments.len(), 1);
        assert!(
            Slack::discover(source(&[("conversations", "C000000001")]), "null".into(), 1).is_err()
        );
        assert!(
            Slack::discover(source(&[("conversations", "C000000004")]), "null".into(), 1).is_err()
        );
    }

    #[test]
    fn explicit_threads_and_history_boundaries_never_expand_selection() {
        let selected = source(&[("threads", "C123456789:1760000002.000001")]);
        let page = Slack::discover(selected, "null".into(), 10).unwrap();
        assert_eq!(page.candidates.len(), 1);
        assert!(page.candidates[0].external_id.ends_with("000002.000001"));
        let future = source(&[("since", "2099-01-01")]);
        assert!(
            Slack::discover(future, "null".into(), 10)
                .unwrap()
                .candidates
                .is_empty()
        );
        assert!(
            Slack::identify(
                Some("T123456789".into()),
                [
                    ("account".into(), "work".into()),
                    ("auth_mode".into(), "bot".into()),
                    ("conversations".into(), "C123456789".into()),
                    ("threads".into(), "D123456789:1760000000.000001".into())
                ]
                .into()
            )
            .is_err()
        );
    }
}
