use axum::http::Method;
use serde_json::Value;

use super::{
    instructions::Format,
    protocol::{Protocol, Usage},
    transcript::Output,
};

const MAX_SSE_EVENT: usize = 1024 * 1024;
const MAX_JSON: usize = 16 * 1024 * 1024;

#[derive(Default)]
pub(super) struct ObservedUsage {
    pub usage: Usage,
    pub model: Option<String>,
    pub raw: serde_json::Map<String, Value>,
    pub output: Option<Output>,
}

pub(super) struct UsageReader {
    protocol: Protocol,
    stream: bool,
    pending: Vec<u8>,
    event_data: Vec<u8>,
    skipped: bool,
    after_cr: bool,
    first_line: bool,
    observed: ObservedUsage,
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
            observed: ObservedUsage::default(),
            terminal: false,
            final_output_reported: false,
            failed: false,
        }
    }

    pub(super) fn capture_output(&mut self, format: Format) {
        self.observed.output = Some(Output::new(format));
    }

    pub(super) fn output(&self) -> Option<&Output> {
        self.observed.output.as_ref()
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
            if std::mem::take(&mut self.skipped) {
                if let Some(output) = &mut self.observed.output {
                    output.mark_truncated();
                }
            } else {
                if self.protocol == Protocol::Codex && data.trim_ascii() == b"[DONE]" {
                    self.terminal = true;
                } else if let Ok(event) = serde_json::from_slice::<Value>(&data) {
                    self.read_event(&event);
                }
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
        if let Some(output) = &mut self.observed.output {
            output.read_event(event);
        }
        match (self.protocol, event["type"].as_str()) {
            (
                Protocol::Codex,
                Some("response.completed" | "response.incomplete" | "response.failed"),
            ) => {
                self.read_response(&event["response"]);
                self.terminal = true;
                self.failed |= event["type"] == "response.failed";
            }
            (Protocol::Codex, Some("response.created" | "response.in_progress")) => {
                self.read_model(&event["response"]);
            }
            (Protocol::Codex, _) if event["object"] == "chat.completion.chunk" => {
                self.read_response(event);
            }
            (Protocol::Claude, Some("message_start")) => self.read_response(&event["message"]),
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

    fn read_model(&mut self, response: &Value) {
        if let Some(model) = response["model"].as_str().filter(|model| !model.is_empty()) {
            self.observed.model = Some(model.to_owned());
        }
    }

    fn read_response(&mut self, response: &Value) {
        self.read_model(response);
        self.read_usage(&response["usage"]);
    }

    fn read_usage(&mut self, usage: &Value) {
        if let Some(input) = usage["input_tokens"]
            .as_u64()
            .or_else(|| usage["prompt_tokens"].as_u64())
        {
            self.observed.usage.input_tokens = Some(input);
        }
        if let Some(output) = usage["output_tokens"]
            .as_u64()
            .or_else(|| usage["completion_tokens"].as_u64())
        {
            self.observed.usage.output_tokens = Some(output);
        }
        if let Some(fields) = usage.as_object() {
            self.observed.raw.extend(fields.clone());
        }
    }

    pub(super) fn finish(mut self, body_complete: bool) -> (ObservedUsage, bool) {
        if !self.stream
            && body_complete
            && !self.skipped
            && let Ok(response) = serde_json::from_slice::<Value>(&self.pending)
        {
            self.read_response(&response);
            if let Some(output) = &mut self.observed.output {
                output.read_json(&response);
            }
            self.terminal = true;
            self.failed = response["type"] == "error" || response["status"] == "failed";
        }
        if let Some(output) = &mut self.observed.output {
            if self.skipped {
                output.mark_truncated();
            }
            output.finish(body_complete && self.terminal);
        }
        self.observed.usage.complete = self.terminal
            && (self.protocol != Protocol::Claude || !self.stream || self.final_output_reported)
            && self.observed.usage.input_tokens.is_some()
            && self.observed.usage.output_tokens.is_some();
        (
            self.observed,
            self.failed || (self.stream && !self.terminal),
        )
    }
}
