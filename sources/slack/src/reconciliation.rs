//! Core owns document inventory. This cursor describes only a bounded history scan.
use super::*;
use exports::memcastle::source::reconciliation::{
    Acquisition, Decision, DiscoveryPage, Guest as ReconciliationGuest, KnownDocument,
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u8,
    // Completed conversation scans only; never advance this over unvisited backlog.
    high: BTreeMap<String, u64>,
    // Freezes the upper boundary for every conversation in this scan.
    scan_at: u64,
    index: usize,
    // Frozen inclusive lower bound and descending exclusive continuation boundary.
    lower: Option<u64>,
    before: u64,
    observed: u64,
    complete: bool,
    // Opaque hints are only for empty pages; timestamp progress remains durable.
    hint: String,
    seen_hints: BTreeSet<String>,
}

fn failed(error: impl std::fmt::Display) -> SourceError {
    SourceError::Failed(error.to_string())
}

fn authenticated(source: &SourceRef) -> Result<(), SourceError> {
    let auth = api::request(source, "auth.test", &[]).map_err(failed)?;
    if auth.get("team_id").and_then(Value::as_str) != Some(&source.locator) {
        return Err(failed(
            "Slack token belongs to a different workspace than the locator",
        ));
    }
    Ok(())
}

fn excluded(source: &SourceRef, id: &str) -> Result<bool, SourceError> {
    let (channel, root) = thread_ref(id).map_err(failed)?;
    let until = option(source, "until")
        .map(date)
        .transpose()
        .map_err(failed)?;
    Ok(!conversations(source).contains(&channel)
        || (option(source, "threads").is_some()
            && !selected_threads(source).contains(&(channel, root.clone())))
        || until.is_some_and(|end| timestamp(&root).is_ok_and(|ts| ts > end)))
}

fn handle(id: &str) -> Result<Handle, SourceError> {
    let (conversation, root) = thread_ref(id).map_err(failed)?;
    Ok(Handle {
        conversation,
        root,
        name: String::new(),
        kind: String::new(),
    })
}

impl ReconciliationGuest for Slack {
    fn reconcile(source: SourceRef, known: KnownDocument) -> Result<Decision, SourceError> {
        authenticated(&source)?;
        if excluded(&source, &known.external_id)? {
            return Ok(Decision::Retire("outside selected Slack membership".into()));
        }
        // Pending entries may have no metadata; retired entries must also be refreshed.
        Ok(Decision::Refresh(
            serde_json::to_string(&handle(&known.external_id)?).unwrap(),
        ))
    }

    fn discover(
        source: SourceRef,
        cursor: String,
        limit: u32,
    ) -> Result<DiscoveryPage, SourceError> {
        discover(source, cursor, limit)
    }

    fn acquire(source: SourceRef, candidate: Candidate) -> Result<Acquisition, SourceError> {
        acquire(source, candidate)
    }
}

pub(super) fn acquire(
    source: SourceRef,
    mut candidate: Candidate,
) -> Result<Acquisition, SourceError> {
    authenticated(&source)?;
    if excluded(&source, &candidate.external_id)? {
        return Ok(Acquisition::Retire(
            "outside selected Slack membership".into(),
        ));
    }
    let mut current = handle(&candidate.external_id)?;
    // Validate discovery handles too: never allow a mismatched opaque handle to select another object.
    let supplied: Handle = serde_json::from_str(&candidate.handle).map_err(failed)?;
    if supplied.conversation != current.conversation || supplied.root != current.root {
        return Err(failed(
            "Slack candidate handle does not match its external ID",
        ));
    }
    let result = (|| {
        (current.name, current.kind) = conversation_info(&source, &current.conversation)?;
        candidate.handle = serde_json::to_string(&current).unwrap();
        read_document(source, candidate)
    })();
    match result {
        Ok(Some(raw)) => Ok(Acquisition::Document(raw)),
        Ok(None) => Ok(Acquisition::Retire(
            "complete Slack thread has no selected messages".into(),
        )),
        Err(error) if matches!(error.kind, api::Failure::AccessLost | api::Failure::Removed) => {
            Ok(Acquisition::Retire(error.to_string()))
        }
        Err(error) if error.kind == api::Failure::Missing => Ok(Acquisition::Skip),
        Err(error) => Err(failed(error)),
    }
}

fn text_time(ts: u64) -> String {
    format!("{}.{:06}", ts / 1_000_000, ts % 1_000_000)
}

fn checkpoint(state: &Cursor) -> String {
    serde_json::to_string(state).unwrap()
}

fn finish(state: &mut Cursor, channel: &str, count: usize) {
    if state.observed > 0 {
        state
            .high
            .entry(channel.into())
            .and_modify(|high| *high = (*high).max(state.observed))
            .or_insert(state.observed);
    }
    state.index += 1;
    state.lower = None;
    state.observed = 0;
    state.hint.clear();
    state.seen_hints.clear();
    state.complete = state.index == count;
}

pub(super) fn discover(
    source: SourceRef,
    position: String,
    limit: u32,
) -> Result<DiscoveryPage, SourceError> {
    authenticated(&source)?;
    if limit == 0 {
        return Err(SourceError::InvalidInput(
            "Slack discovery limit must be positive".into(),
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(failed)?
        .as_secs();
    let invalid =
        || SourceError::CursorInvalid("unknown or invalid Slack cursor; use --full".into());
    let mut state = if position == "null" {
        Cursor {
            version: 1,
            high: BTreeMap::new(),
            scan_at: now,
            index: 0,
            lower: None,
            before: 0,
            observed: 0,
            complete: false,
            hint: String::new(),
            seen_hints: BTreeSet::new(),
        }
    } else {
        serde_json::from_str::<Cursor>(&position).map_err(|_| invalid())?
    };
    let channels = conversations(&source);
    let explicit = selected_threads(&source);
    let count = if option(&source, "threads").is_some() {
        explicit.len()
    } else {
        channels.len()
    };
    if state.version != 1
        || state.scan_at == 0
        || state.index > count
        || state.complete != (state.index == count)
        || state.high.keys().any(|id| !slack_id(id, "CGD"))
        || state.seen_hints.len() > 32
        || (state.lower.is_some() && state.before == 0)
    {
        return Err(invalid());
    }
    if state.complete && now < state.scan_at.saturating_add(60) {
        return Ok(DiscoveryPage {
            candidates: vec![],
            exhausted: true,
            cursor_after: None,
        });
    }
    if state.complete {
        state.index = 0;
        state.scan_at = now;
        state.complete = false;
    }
    let since = option(&source, "since")
        .map(date)
        .transpose()
        .map_err(failed)?
        .unwrap_or(0);
    let until = option(&source, "until")
        .map(date)
        .transpose()
        .map_err(failed)?;
    if option(&source, "threads").is_some() {
        let (channel, root) = &explicit[state.index];
        let ts = timestamp(root).map_err(failed)?;
        state.index += 1;
        state.complete = state.index == count;
        let candidates = if ts >= since && until.is_none_or(|end| ts <= end) {
            vec![Candidate {
                external_id: format!("{channel}:{root}"),
                handle: serde_json::to_string(&handle(&format!("{channel}:{root}"))?).unwrap(),
                cursor_after: checkpoint(&state),
            }]
        } else {
            vec![]
        };
        return Ok(DiscoveryPage {
            candidates,
            exhausted: state.complete,
            cursor_after: Some(checkpoint(&state)),
        });
    }
    let channel = &channels[state.index];
    let (name, kind) = match conversation_info(&source, channel) {
        Ok(info) => info,
        Err(error) if matches!(error.kind, api::Failure::AccessLost | api::Failure::Missing) => {
            finish(&mut state, channel, count);
            return Ok(DiscoveryPage {
                candidates: vec![],
                exhausted: state.complete,
                cursor_after: Some(checkpoint(&state)),
            });
        }
        Err(error) => return Err(failed(error)),
    };
    if state.lower.is_none() {
        let window = option(&source, "revisit_days")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(7)
            * 86_400_000_000;
        state.lower = Some(
            state
                .high
                .get(channel)
                .map_or(since, |high| high.saturating_sub(window).max(since)),
        );
        state.before = until.map_or(state.scan_at.saturating_mul(1_000_000), |end| {
            end.saturating_add(1)
                .min(state.scan_at.saturating_mul(1_000_000))
        });
    }
    let lower = state.lower.unwrap();
    if lower >= state.before {
        finish(&mut state, channel, count);
        return Ok(DiscoveryPage {
            candidates: vec![],
            exhausted: state.complete,
            cursor_after: Some(checkpoint(&state)),
        });
    }
    let oldest = text_time(lower);
    let latest = text_time(state.before - 1);
    let mut params = vec![
        ("channel", channel.as_str()),
        ("oldest", &oldest),
        ("latest", &latest),
        ("inclusive", "true"),
        ("limit", "15"),
    ];
    if !state.hint.is_empty() {
        params.push(("cursor", &state.hint));
    }
    let response = match api::request(&source, "conversations.history", &params) {
        Err(error) if error.kind == api::Failure::InvalidCursor && !state.hint.is_empty() => {
            state.hint.clear();
            return Ok(DiscoveryPage {
                candidates: vec![],
                exhausted: false,
                cursor_after: Some(checkpoint(&state)),
            });
        }
        result => result.map_err(failed)?,
    };
    let messages = response
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| failed("Slack history lacks messages"))?;
    if messages.len() > 15 {
        return Err(failed("Slack history exceeded page bound"));
    }
    let next = response
        .pointer("/response_metadata/next_cursor")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let more = response.get("has_more").and_then(Value::as_bool) == Some(true);
    // Nonempty time-paginated pages advance by their oldest timestamp, even
    // when Slack provides no opaque cursor. Empty pages need a usable hint.
    if more && next.is_empty() && messages.is_empty() {
        return Err(failed(
            "Slack history indicated more results without messages or a cursor",
        ));
    }
    let has_next = more || !next.is_empty();
    let mut prior = state.before;
    for message in messages {
        let (_, ts) = message_ts(message).map_err(failed)?;
        if ts >= prior || ts < lower {
            return Err(failed(
                "Slack history has repeated, unordered or out-of-range timestamps",
            ));
        }
        prior = ts;
    }
    let mut candidates = Vec::new();
    let mut inspected = 0;
    for message in messages {
        if candidates.len() == limit as usize {
            break;
        }
        let (root, ts) = message_ts(message).map_err(failed)?;
        state.before = ts;
        state.observed = state.observed.max(ts);
        state.hint.clear();
        state.seen_hints.clear();
        inspected += 1;
        if retain(message)
            && message
                .get("thread_ts")
                .and_then(Value::as_str)
                .is_none_or(|parent| parent == root)
        {
            candidates.push(Candidate {
                external_id: format!("{channel}:{root}"),
                handle: serde_json::to_string(&Handle {
                    conversation: channel.clone(),
                    root,
                    name: name.clone(),
                    kind: kind.clone(),
                })
                .unwrap(),
                cursor_after: checkpoint(&state),
            });
        }
    }
    if inspected == messages.len() {
        if !has_next {
            finish(&mut state, channel, count);
        } else if messages.is_empty() {
            if state.seen_hints.len() >= 32 || !state.seen_hints.insert(next.into()) {
                return Err(failed("Slack history pagination made no progress"));
            }
            state.hint = next.into();
        }
        if let Some(last) = candidates.last_mut() {
            // The last candidate may also commit trailing exclusions or a completed
            // conversation. Earlier candidates must keep their own prefix checkpoint.
            last.cursor_after = checkpoint(&state);
        }
    }
    Ok(DiscoveryPage {
        candidates,
        exhausted: state.complete,
        cursor_after: Some(checkpoint(&state)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::source;

    const ID: &str = "C123456789:1760000000.000001";

    fn candidate() -> Candidate {
        Candidate {
            external_id: ID.into(),
            cursor_after: "null".into(),
            handle: serde_json::to_string(&handle(ID).unwrap()).unwrap(),
        }
    }

    fn known(pending: bool, retired: bool) -> KnownDocument {
        KnownDocument {
            external_id: ID.into(),
            revision: String::new(),
            metadata: "{}".into(),
            pending,
            retired,
        }
    }

    fn document(scenario: &str) -> RawDocument {
        let account = format!("fixture-{scenario}");
        match acquire(source(&[("account", &account)]), candidate()).unwrap() {
            Acquisition::Document(raw) => raw,
            _ => panic!("expected document for {scenario}"),
        }
    }

    #[test]
    fn inventory_refreshes_pending_and_retired_without_metadata() {
        for (pending, retired) in [(true, false), (false, false), (false, true)] {
            assert!(matches!(
                <Slack as ReconciliationGuest>::reconcile(source(&[]), known(pending, retired))
                    .unwrap(),
                Decision::Refresh(_)
            ));
        }
        // since is a bootstrap bound, never an inventory exclusion.
        assert!(matches!(
            <Slack as ReconciliationGuest>::reconcile(
                source(&[("since", "2099-01-01")]),
                known(false, false)
            )
            .unwrap(),
            Decision::Refresh(_)
        ));
        for options in [
            vec![("conversations", "D123456789")],
            vec![("threads", "C123456789:1760000002.000001")],
            vec![("until", "2020-01-01")],
        ] {
            assert!(matches!(
                <Slack as ReconciliationGuest>::reconcile(source(&options), known(true, false))
                    .unwrap(),
                Decision::Retire(_)
            ));
        }
        let all = source(&[]);
        for (key, value) in [
            ("conversations", "D123456789"),
            ("users", "U987654321"),
            ("threads", ID),
            ("until", "2026-01-01"),
            ("revisit_days", "1"),
        ] {
            assert_eq!(all.account, source(&[(key, value)]).account);
        }
    }

    #[test]
    fn edits_and_deleted_replies_refresh_whole_document() {
        let original = document("work");
        let edited = document("edit");
        let deleted = document("deleted-reply");
        assert_ne!(original.revision, edited.revision);
        assert!(edited.body.contains("Ship on Monday"));
        assert_eq!(Slack::normalize(deleted).unwrap().segments.len(), 1);
        assert_eq!(original.revision, document("work").revision);
        assert_eq!(original.revision, document("archived").revision);
    }

    #[test]
    fn complete_multipage_threads_include_later_edits_and_deletions() {
        let original = document("paged-replies");
        let edited = document("paged-replies-edit");
        let deleted = document("paged-replies-deleted");
        assert!(original.body.contains("Paged reply 20"));
        assert!(edited.body.contains("Edited later-page reply"));
        assert!(!edited.body.contains("Paged reply 20"));
        assert!(!deleted.body.contains("Paged reply 20"));
        assert_ne!(original.revision, edited.revision);
        assert_ne!(original.revision, deleted.revision);
        assert_eq!(Slack::normalize(original).unwrap().segments.len(), 22);
        assert_eq!(Slack::normalize(edited).unwrap().segments.len(), 22);
        assert_eq!(Slack::normalize(deleted).unwrap().segments.len(), 21);
    }

    #[test]
    fn retire_only_with_evidence_and_never_on_global_failures() {
        for scenario in ["removed", "lost-access"] {
            assert!(matches!(
                acquire(
                    source(&[("account", &format!("fixture-{scenario}"))]),
                    candidate()
                )
                .unwrap(),
                Acquisition::Retire(_)
            ));
        }
        for scenario in ["empty-thread", "missing-thread", "retention"] {
            assert!(matches!(
                acquire(
                    source(&[("account", &format!("fixture-{scenario}"))]),
                    candidate()
                )
                .unwrap(),
                Acquisition::Skip
            ));
        }
        for scenario in [
            "invalid",
            "wrong-workspace",
            "missing-scope",
            "rate-limit",
            "http-forbidden",
            "partial",
            "truncated",
            "repeat-page",
            "failed-first",
        ] {
            assert!(
                acquire(
                    source(&[("account", &format!("fixture-{scenario}"))]),
                    candidate()
                )
                .is_err(),
                "{scenario}"
            );
        }
        assert!(matches!(
            acquire(source(&[("users", "U000000001")]), candidate()).unwrap(),
            Acquisition::Retire(_)
        ));
        let until = source(&[("until", "1760000000.000001")]);
        let Acquisition::Document(raw) = acquire(until, candidate()).unwrap() else {
            panic!()
        };
        assert_eq!(Slack::normalize(raw).unwrap().segments.len(), 1);
    }

    #[test]
    fn filtered_pages_and_one_document_restarts_do_not_skip_backlog() {
        for account in ["fixture-backlog", "fixture-time-only"] {
            let src = source(&[("account", account)]);
            let first = discover(src.clone(), "null".into(), 1).unwrap();
            assert!(first.candidates.is_empty());
            assert!(!first.exhausted);
            let mut position = first.cursor_after.unwrap();
            let saved: Cursor = serde_json::from_str(&position).unwrap();
            assert!(saved.high.is_empty());
            let mut ids = BTreeSet::new();
            for _ in 0..25 {
                let page = discover(src.clone(), position.clone(), 1).unwrap();
                // Replaying the same checkpoint (interruption before commit) is deterministic.
                let replay = discover(src.clone(), position, 1).unwrap();
                assert_eq!(page.cursor_after, replay.cursor_after);
                for candidate in page.candidates {
                    assert!(ids.insert(candidate.external_id));
                }
                position = page.cursor_after.unwrap();
                if page.exhausted {
                    break;
                }
                let state: Cursor = serde_json::from_str(&position).unwrap();
                assert!(
                    state.high.is_empty(),
                    "high water must not commit before backlog completion"
                );
                assert_eq!(state.lower, saved.lower);
            }
            assert_eq!(ids.len(), 20);
            let state: Cursor = serde_json::from_str(&position).unwrap();
            assert!(state.complete);
            assert_eq!(state.high["C123456789"], 1_760_000_039_000_001);
        }
        let filtered = source(&[("account", "fixture-filtered")]);
        let mut position = String::from("null");
        for n in 0..3 {
            let page = discover(filtered.clone(), position.clone(), 1).unwrap();
            assert!(page.candidates.is_empty());
            let after = page.cursor_after.unwrap();
            assert_ne!(after, position);
            assert_eq!(page.exhausted, n == 2);
            position = after;
        }
    }

    #[test]
    fn empty_pages_and_expired_hints_are_bounded_and_restartable() {
        let src = source(&[("account", "fixture-expired-cursor")]);
        let first = discover(src.clone(), "null".into(), 1).unwrap();
        let second = discover(src.clone(), first.cursor_after.unwrap(), 1).unwrap();
        let state: Cursor = serde_json::from_str(second.cursor_after.as_ref().unwrap()).unwrap();
        assert!(state.hint.is_empty());
        assert_eq!(state.observed, 0);
        // A second identical no-progress hint is rejected rather than looping forever.
        assert!(discover(src, second.cursor_after.unwrap(), 1).is_err());
        let src = source(&[("account", "fixture-empty-continuation")]);
        let first = discover(src.clone(), "null".into(), 1).unwrap();
        let second = discover(src, first.cursor_after.unwrap(), 1).unwrap();
        assert!(second.cursor_after.is_some());
        assert!(second.candidates.is_empty()); // first real page contains only service events
    }
}
