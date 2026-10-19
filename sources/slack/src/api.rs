//! Slack Web API: bearer credentials never appear in URLs, process arguments or errors.

use serde_json::Value;

use crate::SourceRef;

#[cfg(not(test))]
fn run_process(
    program: &str,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<crate::memcastle::source::host::ProcessOutput, String> {
    crate::memcastle::source::host::run_process(program, args, stdin)
        .map_err(|_| "cannot run curl on the daemon's PATH".into())
}

#[cfg(test)]
fn run_process(
    _program: &str,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<crate::memcastle::source::host::ProcessOutput, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/bin/curl"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input)
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    Ok(crate::memcastle::source::host::ProcessOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

// Core resolves the selected configuration's env/file reference before executing the job.
// Conformance has no job credential: its offline curl stand-in accepts an absent header,
// while Slack itself returns `not_authed` and cannot provide any live data without one.
#[cfg(not(test))]
fn token(_source: &SourceRef) -> Option<String> {
    crate::memcastle::source::host::source_credential().ok()
}

#[cfg(test)]
fn token(source: &SourceRef) -> Option<String> {
    Some(
        match crate::option(source, "account") {
            Some("other-bot") => "fixture-other-bot",
            Some("personal-user") if source.locator == "T987654321" => "fixture-personal",
            _ => "fixture-work",
        }
        .into(),
    )
}

fn quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("invalid Slack HTTP parameter".into());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn encode(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

pub(crate) fn request(
    source: &SourceRef,
    method: &str,
    params: &[(&str, &str)],
) -> Result<Value, String> {
    if !matches!(
        method,
        "auth.test" | "conversations.info" | "conversations.history" | "conversations.replies"
    ) {
        return Err("unsupported Slack API method".into());
    }
    let mut address = format!("https://slack.com/api/{method}");
    for (index, (key, value)) in params.iter().enumerate() {
        if !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return Err("invalid Slack API parameter".into());
        }
        address.push(if index == 0 { '?' } else { '&' });
        address.push_str(key);
        address.push('=');
        address.push_str(&encode(value));
    }
    let mut config = format!(
        "url = \"{}\"\nheader = \"Accept: application/json\"\n",
        quote(&address)?
    );
    if let Some(secret) = token(source) {
        config.push_str(&format!(
            "header = \"Authorization: Bearer {}\"\n",
            quote(&secret)?
        ));
    }
    let output = run_process(
        "curl",
        &[
            "-q".into(),
            "--silent".into(),
            "--show-error".into(),
            "--config".into(),
            "-".into(),
            "--max-time".into(),
            "30".into(),
            "--max-redirs".into(),
            "0".into(),
            "--dump-header".into(),
            "-".into(),
            "--write-out".into(),
            "\nMEMCASTLE_HTTP_STATUS:%{http_code}".into(),
        ],
        Some(config.as_bytes()),
    )?;
    if output.status != 0 {
        return Err(
            "Slack request failed; check connectivity, token and daemon curl installation".into(),
        );
    }
    let output = String::from_utf8(output.stdout).map_err(|_| "Slack response was not UTF-8")?;
    let (payload, status) = output
        .rsplit_once("\nMEMCASTLE_HTTP_STATUS:")
        .ok_or("Slack response lacked HTTP status")?;
    // curl emits headers before the response body; never include bodies/headers in errors.
    let (headers, body) = payload
        .rsplit_once("\r\n\r\n")
        .or_else(|| payload.rsplit_once("\n\n"))
        .ok_or("Slack response lacked HTTP headers")?;
    match status.trim() {
        "200" => {}
        "429" => {
            let retry = headers.lines().rev().find_map(|line| {
                line.trim()
                    .to_ascii_lowercase()
                    .strip_prefix("retry-after:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            });
            return Err(match retry {
                Some(seconds) => format!("Slack rate limit reached; retry after {seconds} seconds"),
                None => "Slack rate limit reached; retry later".into(),
            });
        }
        "401" | "403" => {
            return Err(
                "Slack denied the request; check token, scopes and conversation access".into(),
            );
        }
        "404" => return Err("Slack conversation is unavailable or inaccessible".into()),
        code => {
            return Err(format!(
                "Slack API returned HTTP {code}; retry or check API availability"
            ));
        }
    }
    let response: Value =
        serde_json::from_str(body).map_err(|_| "Slack returned malformed JSON")?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        let error = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // Strict allowlist: Slack's error field is service data and must not leak private content.
        let message = match error {
            "missing_scope" => "missing_scope: grant the required conversation read/history scope",
            "not_in_channel" => "not_in_channel: join or select an accessible conversation",
            "channel_not_found" | "invalid_channel" => {
                "conversation unavailable, deleted or inaccessible"
            }
            "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" => {
                "Slack token is missing, invalid or expired; configure an env or file credential on the selected source configuration"
            }
            "no_permission" | "ekm_access_denied" => "Slack token has insufficient permissions",
            "ratelimited" => "Slack rate limit reached; retry later",
            _ => "Slack API refused the request; inspect app permissions and selected conversation",
        };
        return Err(message.into());
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_and_urls_are_not_reflected_in_errors() {
        assert!(quote("line\nbreak").is_err());
        assert_eq!(encode("x:1.2"), "x%3A1.2");
        let source = crate::tests::source(&[]);
        assert!(request(&source, "invalid", &[]).is_err());
        assert!(request(&source, "conversations.info", &[("bad\nheader", "value")]).is_err());
        let error =
            request(&source, "conversations.info", &[("channel", "C000000001")]).unwrap_err();
        assert!(error.contains("inaccessible"), "{error}");
        assert!(!error.contains("fixture-work"));
        let limited =
            request(&source, "conversations.info", &[("channel", "C000000003")]).unwrap_err();
        assert!(limited.contains("60 seconds"), "{limited}");
        let permission =
            request(&source, "conversations.info", &[("channel", "C000000002")]).unwrap_err();
        assert!(permission.contains("missing_scope"), "{permission}");
    }
}
