use axum::http::Method;
use serde_json::Value;

use super::protocol::{Protocol, Usage};

const MAX_SSE_EVENT: usize = 1024 * 1024;
const MAX_JSON: usize = 16 * 1024 * 1024;

pub(super) struct UsageReader {
    protocol: Protocol,
    stream: bool,
    pending: Vec<u8>,
    event_data: Vec<u8>,
    skipped: bool,
    after_cr: bool,
    first_line: bool,
    usage: Usage,
    terminal: bool,
    final_output_reported: bool,
    failed: bool,
}

impl UsageReader {
    pub(super) fn tracks(method: &Method, path: &str) -> bool {
        method == Method::POST
            && !matches!(path, "/responses/input_tokens" | "/messages/count_tokens")
    }

    pub(super) fn new(protocol: Protocol, stream: bool) -> Self {
        Self {
            protocol,
            stream,
            pending: Vec::new(),
            event_data: Vec::new(),
            skipped: false,
            after_cr: false,
            first_line: true,
            usage: Usage::default(),
            terminal: false,
            final_output_reported: false,
            failed: false,
        }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) {
        if !self.stream {
            if self.pending.len() + chunk.len() > MAX_JSON {
                self.pending.clear();
                self.skipped = true;
            } else if !self.skipped {
                self.pending.extend_from_slice(chunk);
            }
            return;
        }

        for &byte in chunk {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            if matches!(byte, b'\r' | b'\n') {
                self.read_line();
                self.after_cr = byte == b'\r';
            } else if self.pending.len() <= MAX_SSE_EVENT {
                self.pending.push(byte);
            }
        }
    }

    fn read_line(&mut self) {
        let line = if std::mem::take(&mut self.first_line) {
            self.pending
                .strip_prefix(b"\xef\xbb\xbf")
                .unwrap_or(&self.pending)
        } else {
            &self.pending
        };
        if line.is_empty() {
            let mut data = std::mem::take(&mut self.event_data);
            if !std::mem::take(&mut self.skipped)
                && let Ok(event) = serde_json::from_slice::<Value>(&data)
            {
                self.read_event(&event);
            }
            data.clear();
            self.event_data = data;
        } else if !self.skipped
            && let Some(data) = line
                .strip_prefix(b"data:")
                .or_else(|| (line == b"data").then_some(&[][..]))
        {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if self.event_data.len() + data.len() >= MAX_SSE_EVENT {
                self.skipped = true;
            } else {
                self.event_data.extend_from_slice(data);
                self.event_data.push(b'\n');
            }
        }
        self.pending.clear();
    }

    fn read_event(&mut self, event: &Value) {
        match (self.protocol, event["type"].as_str()) {
            (
                Protocol::Codex,
                Some("response.completed" | "response.incomplete" | "response.failed"),
            ) => {
                self.read_usage(&event["response"]["usage"]);
                self.terminal = true;
                self.failed |= event["type"] == "response.failed";
            }
            (Protocol::Claude, Some("message_start")) => {
                self.read_usage(&event["message"]["usage"])
            }
            (Protocol::Claude, Some("message_delta")) => {
                let usage = &event["usage"];
                self.final_output_reported |= usage["output_tokens"].as_u64().is_some();
                self.read_usage(usage);
            }
            (Protocol::Claude, Some("message_stop")) => self.terminal = true,
            (_, Some("error")) => self.failed = true,
            _ => {}
        }
    }

    fn read_usage(&mut self, usage: &Value) {
        if let Some(input) = usage["input_tokens"].as_u64() {
            self.usage.input_tokens = Some(input);
        }
        if let Some(output) = usage["output_tokens"].as_u64() {
            self.usage.output_tokens = Some(output);
        }
    }

    pub(super) fn finish(mut self, body_complete: bool) -> (Usage, bool) {
        if !self.stream
            && body_complete
            && !self.skipped
            && let Ok(response) = serde_json::from_slice::<Value>(&self.pending)
        {
            self.read_usage(&response["usage"]);
            self.terminal = true;
            self.failed = response["type"] == "error" || response["status"] == "failed";
        }
        self.usage.complete = self.terminal
            && (self.protocol != Protocol::Claude || !self.stream || self.final_output_reported)
            && self.usage.input_tokens.is_some()
            && self.usage.output_tokens.is_some();
        (self.usage, self.failed || (self.stream && !self.terminal))
    }
}
