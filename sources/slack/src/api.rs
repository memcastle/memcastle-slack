//! Slack Web API: bearer credentials never appear in URLs, process arguments or errors.

use serde_json::Value;

use crate::SourceRef;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Other,
    Authentication,
    MissingScope,
    RateLimited,
    Permission,
    Transport,
    AccessLost,
    Removed,
    Missing,
    InvalidCursor,
}

#[derive(Debug)]
pub(crate) struct Error {
    pub kind: Failure,
    message: String,
}

impl Error {
    pub(crate) fn removed() -> Self {
        Self::new(
            Failure::Removed,
            "Slack root has an explicit deletion marker",
        )
    }
    pub(crate) fn missing(message: &str) -> Self {
        Self::new(Failure::Missing, message)
    }
    fn new(kind: Failure, message: &str) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self {
            kind: Failure::Other,
            message,
        }
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

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
// Generic conformance supplies Core's fixed synthetic selected credential.
#[cfg(not(test))]
fn token(_source: &SourceRef) -> Result<String, Error> {
    crate::memcastle::source::host::source_credential().map_err(|_| {
        Error::new(
            Failure::Authentication,
            "selected Slack credential is unavailable",
        )
    })
}

#[cfg(test)]
fn token(source: &SourceRef) -> Result<String, Error> {
    Ok(match crate::option(source, "account") {
        Some(scenario) if scenario.starts_with("fixture-") => scenario,
        Some("other-bot") => "fixture-other-bot",
        Some("personal-user") if source.locator == "T987654321" => "fixture-personal",
        _ => "fixture-work",
    }
    .into())
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
) -> Result<Value, Error> {
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
    {
        let secret = token(source)?;
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
    )
    .map_err(|message| Error::new(Failure::Transport, &message))?;
    if output.status != 0 {
        return Err(Error::new(
            Failure::Transport,
            "Slack request failed; check connectivity, token and daemon curl installation",
        ));
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
            return Err(Error::new(
                Failure::RateLimited,
                &match retry {
                    Some(seconds) => {
                        format!("Slack rate limit reached; retry after {seconds} seconds")
                    }
                    None => "Slack rate limit reached; retry later".into(),
                },
            ));
        }
        "401" => {
            return Err(Error::new(
                Failure::Authentication,
                "Slack denied authentication; check the selected token",
            ));
        }
        "403" => {
            return Err(Error::new(
                Failure::Permission,
                "Slack denied the request; check token, scopes and conversation access",
            ));
        }
        "404" => return Err("Slack conversation is unavailable or inaccessible".into()),
        code => {
            return Err(
                format!("Slack API returned HTTP {code}; retry or check API availability").into(),
            );
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
        let (kind, message) = match error {
            "not_in_channel" => (
                Failure::AccessLost,
                "not_in_channel: conversation access lost",
            ),
            "channel_not_found" | "thread_not_found" => {
                (Failure::Missing, "Slack object is absent or inaccessible")
            }
            "invalid_cursor" => (Failure::InvalidCursor, "Slack pagination cursor expired"),
            "missing_scope" => (
                Failure::MissingScope,
                "missing_scope: grant the required conversation read/history scope",
            ),
            "invalid_auth" | "not_authed" | "token_revoked" | "token_expired" => (
                Failure::Authentication,
                "Slack token is missing, invalid or expired; configure an env or file credential on the selected source configuration",
            ),
            "no_permission" | "ekm_access_denied" => (
                Failure::Permission,
                "Slack token has insufficient permissions",
            ),
            "ratelimited" => (
                Failure::RateLimited,
                "Slack rate limit reached; retry later",
            ),
            _ => (
                Failure::Other,
                "Slack API refused the request; inspect app permissions and selected conversation",
            ),
        };
        return Err(Error::new(kind, message));
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
        assert!(error.to_string().contains("inaccessible"), "{error}");
        assert!(!error.to_string().contains("fixture-work"));
        let limited =
            request(&source, "conversations.info", &[("channel", "C000000003")]).unwrap_err();
        assert!(limited.to_string().contains("60 seconds"), "{limited}");
        let permission =
            request(&source, "conversations.info", &[("channel", "C000000002")]).unwrap_err();
        assert!(
            permission.to_string().contains("missing_scope"),
            "{permission}"
        );
    }
}
