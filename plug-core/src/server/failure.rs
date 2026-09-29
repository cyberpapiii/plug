//! Why a server failed: a short, redacted reason for status output, and the
//! tail of what a stdio server wrote to stderr.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::config::ServerConfig;

const REDACTED: &str = "<redacted>";

/// Longest reason status output shows.
const MAX_REASON_CHARS: usize = 300;

/// How much of a stdio server's stderr is kept.
const STDERR_TAIL_BYTES: usize = 4096;

/// Longest single stderr line kept; the rest of the line is dropped.
const STDERR_LINE_BYTES: usize = 512;

/// How long a failed start waits for the server's stderr to close, so the
/// lines it wrote just before exiting are in the report.
const STDERR_SETTLE: Duration = Duration::from_millis(250);

/// Values from the server's own config that must never reach a log or status
/// line: its env values and bearer token. Short values are skipped because
/// they would redact ordinary words.
pub(crate) fn config_secrets(config: &ServerConfig) -> Vec<String> {
    config
        .env
        .values()
        .map(String::as_str)
        .chain(config.auth_token.as_ref().map(|token| token.as_str()))
        .filter(|value| value.len() >= 8)
        .map(|value| value.to_string())
        .collect()
}

/// One line describing `error` and its causes, with secrets removed and the
/// length bounded.
pub(crate) fn summarize_error(error: &anyhow::Error, secrets: &[String]) -> String {
    let one_line = format!("{error:#}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&redact(&one_line, secrets), MAX_REASON_CHARS)
}

/// Remove the server's own secrets, URL query strings, and anything that looks
/// like a token.
pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut text = text.to_string();
    for secret in secrets {
        text = text.replace(secret.as_str(), REDACTED);
    }
    redact_token_runs(&strip_url_queries(&text))
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max - 1).collect();
    truncated.push('…');
    truncated
}

fn strip_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = ["https://", "http://"]
        .iter()
        .filter_map(|scheme| rest.find(scheme))
        .min()
    {
        let (before, from_url) = rest.split_at(start);
        out.push_str(before);
        let end = from_url
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | ')'))
            .unwrap_or(from_url.len());
        let url = &from_url[..end];
        match url.find('?') {
            Some(query) => {
                out.push_str(&url[..=query]);
                out.push_str(REDACTED);
            }
            None => out.push_str(url),
        }
        rest = &from_url[end..];
    }
    out.push_str(rest);
    out
}

/// Replace runs of `[A-Za-z0-9_-]` long enough and mixed enough to be an API
/// key or token.
fn redact_token_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run_start = None;
    for (index, c) in text.char_indices() {
        let in_run = c.is_ascii_alphanumeric() || c == '_' || c == '-';
        match (in_run, run_start) {
            (true, None) => run_start = Some(index),
            (true, Some(_)) => {}
            (false, Some(start)) => {
                push_run(&mut out, &text[start..index]);
                run_start = None;
                out.push(c);
            }
            (false, None) => out.push(c),
        }
    }
    if let Some(start) = run_start {
        push_run(&mut out, &text[start..]);
    }
    out
}

fn push_run(out: &mut String, run: &str) {
    let looks_like_token = run.len() >= 24
        && run.bytes().any(|b| b.is_ascii_digit())
        && run.bytes().any(|b| b.is_ascii_alphabetic());
    out.push_str(if looks_like_token { REDACTED } else { run });
}

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const RETIRING: u8 = 2;

/// The last few KB a stdio server wrote to stderr.
///
/// A task drains the pipe for the life of the process, so a chatty server can
/// never block on a full pipe. Nothing is logged while the server runs. If it
/// stops on its own after starting, the tail is logged; a failed start reports
/// it through [`StderrTail::explain`] instead.
pub(crate) struct StderrTail {
    lines: Mutex<Lines>,
    phase: AtomicU8,
    closed: tokio::sync::watch::Sender<bool>,
    secrets: Vec<String>,
}

#[derive(Default)]
struct Lines {
    lines: VecDeque<String>,
    bytes: usize,
}

impl StderrTail {
    pub(crate) fn capture(
        server: &str,
        stderr: impl AsyncRead + Unpin + Send + 'static,
        secrets: Vec<String>,
    ) -> Arc<Self> {
        let (closed, _) = tokio::sync::watch::channel(false);
        let tail = Arc::new(Self {
            lines: Mutex::new(Lines::default()),
            phase: AtomicU8::new(STARTING),
            closed,
            secrets,
        });
        let server = server.to_string();
        let drain = Arc::clone(&tail);
        tokio::spawn(async move {
            drain.drain(stderr).await;
            drain.closed.send_replace(true);
            if drain.phase.load(Ordering::Acquire) == RUNNING {
                let text = drain.text();
                tracing::warn!(
                    server = %server,
                    stderr = %text,
                    "stdio server stopped on its own"
                );
            }
        });
        tail
    }

    async fn drain(&self, stderr: impl AsyncRead + Unpin) {
        let mut reader = BufReader::new(stderr);
        let mut partial = Vec::new();
        loop {
            let chunk = match reader.fill_buf().await {
                Ok([]) | Err(_) => break,
                Ok(chunk) => chunk,
            };
            let len = chunk.len();
            for &byte in chunk {
                if byte == b'\n' {
                    self.push(&partial);
                    partial.clear();
                } else if partial.len() < STDERR_LINE_BYTES {
                    partial.push(byte);
                }
            }
            reader.consume(len);
        }
        self.push(&partial);
    }

    fn push(&self, line: &[u8]) {
        let line = String::from_utf8_lossy(line).trim_end().to_string();
        if line.trim().is_empty() {
            return;
        }
        let mut lines = self.lines.lock().expect("stderr tail mutex poisoned");
        lines.bytes += line.len();
        lines.lines.push_back(line);
        while lines.bytes > STDERR_TAIL_BYTES {
            match lines.lines.pop_front() {
                Some(dropped) => lines.bytes -= dropped.len(),
                None => break,
            }
        }
    }

    /// Everything kept, redacted, one line per stderr line.
    pub(crate) fn text(&self) -> String {
        let lines = self.lines.lock().expect("stderr tail mutex poisoned");
        let joined = lines.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        redact(&joined, &self.secrets)
    }

    fn last_line(&self) -> Option<String> {
        let lines = self.lines.lock().expect("stderr tail mutex poisoned");
        lines
            .lines
            .back()
            .map(|line| truncate_chars(&redact(line, &self.secrets), 200))
    }

    /// The server finished starting: from now on, stopping on its own is
    /// worth a warning.
    pub(crate) fn mark_running(&self) {
        let _ = self
            .phase
            .compare_exchange(STARTING, RUNNING, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Plug is stopping the server, so its exit is expected.
    pub(crate) fn mark_retiring(&self) {
        self.phase.store(RETIRING, Ordering::Release);
    }

    /// Add what the server said on stderr to a start failure: its last line
    /// into the error, and the whole tail into a warning.
    pub(crate) async fn explain(&self, server: &str, error: anyhow::Error) -> anyhow::Error {
        self.mark_retiring();
        let mut closed = self.closed.subscribe();
        let _ = tokio::time::timeout(STDERR_SETTLE, closed.wait_for(|closed| *closed)).await;
        let Some(last_line) = self.last_line() else {
            return error;
        };
        tracing::warn!(
            server = %server,
            stderr = %self.text(),
            "stdio server wrote to stderr before it failed to start"
        );
        anyhow::anyhow!("{error:#} (stderr: {last_line})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn summary_is_one_bounded_line() {
        let error = anyhow::anyhow!("inner\n  detail").context("outer");
        assert_eq!(summarize_error(&error, &[]), "outer: inner detail");

        let long = anyhow::anyhow!("{}", "word ".repeat(200));
        let summary = summarize_error(&long, &[]);
        assert_eq!(summary.chars().count(), MAX_REASON_CHARS);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn redaction_removes_config_secrets_queries_and_tokens() {
        let text = "auth failed for hunter2hunter2 at https://mcp.example.com/mcp?api_key=abc&x=1 \
                    with figd_4f9A8b7C6d5E4f3A2b1C0d9E8f7A and path /opt/homebrew/bin/npx";
        let redacted = redact(text, &secrets(&["hunter2hunter2"]));
        assert_eq!(
            redacted,
            "auth failed for <redacted> at https://mcp.example.com/mcp?<redacted> \
             with <redacted> and path /opt/homebrew/bin/npx"
        );
    }

    #[test]
    fn redaction_keeps_ordinary_error_text() {
        let text = "failed to spawn `npx`: not found on the login shell PATH (os error 2)";
        assert_eq!(redact(text, &[]), text);
    }

    #[test]
    fn config_secrets_skip_short_values() {
        let mut config: ServerConfig = toml::from_str(
            r#"
            command = "npx"
            auth_token = "bearer-token-value"
            "#,
        )
        .expect("config");
        config.env.insert("DEBUG".into(), "1".into());
        config
            .env
            .insert("FIGMA_ACCESS_TOKEN".into(), "figd_secret_value".into());
        let mut found = config_secrets(&config);
        found.sort();
        assert_eq!(found, secrets(&["bearer-token-value", "figd_secret_value"]));
    }

    #[tokio::test]
    async fn tail_keeps_the_last_lines_within_its_bound() {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let tail = StderrTail::capture("test", reader, Vec::new());
        let mut input = String::new();
        for index in 0..1000 {
            input.push_str(&format!("line {index}\n"));
        }
        input.push_str(&"x".repeat(10_000));
        tokio::io::AsyncWriteExt::write_all(&mut writer, input.as_bytes())
            .await
            .expect("write");
        drop(writer);
        let mut closed = tail.closed.subscribe();
        closed.wait_for(|closed| *closed).await.expect("closed");

        let text = tail.text();
        assert!(text.len() <= STDERR_TAIL_BYTES + 1000, "{}", text.len());
        assert!(text.contains("line 999"));
        assert!(!text.contains("line 1\n"));
        assert_eq!(
            tail.last_line().map(|line| line.chars().count()),
            Some(200),
            "an unterminated long line is kept, bounded"
        );
    }

    #[tokio::test]
    async fn explain_adds_the_last_stderr_line_and_redacts_it() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let tail = StderrTail::capture("figma", reader, secrets(&["figd_secret_value"]));
        tokio::io::AsyncWriteExt::write_all(
            &mut writer,
            b"starting\nenv: node: No such file or directory token=figd_secret_value\n",
        )
        .await
        .expect("write");
        drop(writer);

        let error = tail
            .explain("figma", anyhow::anyhow!("failed to initialize client"))
            .await;
        assert_eq!(
            error.to_string(),
            "failed to initialize client (stderr: env: node: No such file or directory \
             token=<redacted>)"
        );
    }

    #[tokio::test]
    async fn explain_leaves_the_error_alone_without_stderr() {
        let (writer, reader) = tokio::io::duplex(1024);
        let tail = StderrTail::capture("quiet", reader, Vec::new());
        drop(writer);
        let error = tail.explain("quiet", anyhow::anyhow!("timed out")).await;
        assert_eq!(error.to_string(), "timed out");
    }
}
