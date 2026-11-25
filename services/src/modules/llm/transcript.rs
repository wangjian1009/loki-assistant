use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Map, Value};

use super::instructions::Format;

const MAX_TEXT: usize = 256 * 1024;
const MAX_BLOCKS: usize = 128;

#[derive(Default, Serialize)]
struct Block {
    kind: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    role: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    id: String,
    text: String,
}

#[derive(Serialize)]
pub(super) struct Input {
    settings: Map<String, Value>,
    tools: Vec<Block>,
    blocks: Vec<Block>,
    truncated: bool,
}

impl Input {
    pub(super) fn read(format: Format, request: &Value) -> Self {
        let mut settings = Map::new();
        for field in [
            "stream",
            "temperature",
            "top_p",
            "max_tokens",
            "max_completion_tokens",
            "max_output_tokens",
            "reasoning",
            "tool_choice",
            "parallel_tool_calls",
        ] {
            if let Some(value) = request.get(field) {
                let serialized = value.to_string();
                if serialized.len() <= 4096 {
                    settings.insert(field.into(), value.clone());
                } else {
                    settings.insert(field.into(), "…".into());
                }
            }
        }
        let mut remaining = MAX_TEXT;
        let mut truncated = false;
        let mut tools = Vec::new();
        for tool in array(&request["tools"]).iter().take(MAX_BLOCKS) {
            let name = text(&tool["name"])
                .or_else(|| text(&tool["function"]["name"]))
                .or_else(|| text(&tool["type"]))
                .unwrap_or("tool");
            tools.push(Block {
                kind: "tool_definition",
                name: name.chars().take(128).collect(),
                text: bounded(&tool.to_string(), &mut remaining, &mut truncated),
                ..Block::default()
            });
            if remaining == 0 {
                break;
            }
        }
        truncated |= tools.len() < array(&request["tools"]).len();
        let mut blocks = match format {
            Format::Responses => match request["input"].as_str() {
                Some(input) => vec![message("user", input.into())],
                None => array(&request["input"])
                    .iter()
                    .flat_map(response_item)
                    .collect(),
            },
            Format::ChatCompletions | Format::Messages => array(&request["messages"])
                .iter()
                .flat_map(|value| conversation_message(format, value))
                .collect(),
        };
        // 大段历史上下文常被重复发送，保留最新消息以便定位当前请求。
        if blocks.len() > MAX_BLOCKS {
            blocks.drain(..blocks.len() - MAX_BLOCKS);
            truncated = true;
        }
        let mut remaining = MAX_TEXT;
        for block in blocks.iter_mut().rev() {
            block.text = bounded(&block.text, &mut remaining, &mut truncated);
        }
        blocks.retain(|block| !block.text.is_empty() || block.kind != "message");
        Self {
            settings,
            tools,
            blocks,
            truncated,
        }
    }
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.is_empty())
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(content)
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => value.to_string(),
    }
}

fn content(block: &Value) -> String {
    if let Some(value) = block.as_str() {
        return value.into();
    }
    match block["type"].as_str() {
        Some("text" | "input_text" | "output_text") => value_text(&block["text"]),
        Some("refusal") => value_text(&block["refusal"]),
        Some("image" | "image_url" | "input_image") => "［图片］".into(),
        Some("document" | "file" | "input_file") => "［文件］".into(),
        Some("audio" | "input_audio") => "［音频］".into(),
        _ => String::new(),
    }
}

fn message(role: &str, text: String) -> Block {
    Block {
        kind: "message",
        role: role.into(),
        text,
        ..Block::default()
    }
}

fn tool(kind: &'static str, value: &Value, id: &Value, body: &Value) -> Block {
    Block {
        kind,
        name: text(&value["name"])
            .or_else(|| text(&value["type"]))
            .unwrap_or_default()
            .chars()
            .take(128)
            .collect(),
        id: text(id).unwrap_or_default().chars().take(128).collect(),
        text: value_text(body),
        ..Block::default()
    }
}

fn response_item(item: &Value) -> Vec<Block> {
    match item["type"].as_str() {
        Some("message") | None if item["role"].is_string() => {
            let role = item["role"].as_str().unwrap();
            if matches!(role, "system" | "developer") {
                return Vec::new();
            }
            let content = value_text(&item["content"]);
            (!content.is_empty())
                .then(|| message(role, content))
                .into_iter()
                .collect()
        }
        Some(
            "function_call"
            | "custom_tool_call"
            | "mcp_call"
            | "web_search_call"
            | "file_search_call"
            | "code_interpreter_call"
            | "computer_call"
            | "shell_call",
        ) => {
            let id = item.get("call_id").unwrap_or(&item["id"]);
            let args = item
                .get("arguments")
                .or_else(|| item.get("input"))
                .or_else(|| item.get("action"))
                .or_else(|| item.get("queries"))
                .unwrap_or(&Value::Null);
            let mut blocks = vec![tool("tool_call", item, id, args)];
            if let Some(output) = item
                .get("output")
                .or_else(|| item.get("outputs"))
                .or_else(|| item.get("error"))
                .filter(|value| !value.is_null())
            {
                blocks.push(tool("tool_result", item, id, output));
            }
            blocks
        }
        Some(
            "function_call_output"
            | "custom_tool_call_output"
            | "computer_call_output"
            | "shell_call_output",
        ) => vec![tool("tool_result", item, &item["call_id"], &item["output"])],
        _ => Vec::new(),
    }
}

fn conversation_message(format: Format, value: &Value) -> Vec<Block> {
    let Some(role) = text(&value["role"]) else {
        return Vec::new();
    };
    if matches!(role, "system" | "developer") {
        return Vec::new();
    }
    if role == "tool" {
        return vec![tool(
            "tool_result",
            value,
            &value["tool_call_id"],
            &value["content"],
        )];
    }
    let mut blocks = Vec::new();
    if matches!(format, Format::Messages) {
        for block in array(&value["content"]) {
            match block["type"].as_str() {
                Some("tool_use" | "server_tool_use") => {
                    blocks.push(tool("tool_call", block, &block["id"], &block["input"]))
                }
                Some(kind) if kind == "tool_result" || kind.ends_with("_tool_result") => blocks
                    .push(tool(
                        "tool_result",
                        block,
                        &block["tool_use_id"],
                        &block["content"],
                    )),
                _ => {
                    let value = content(block);
                    if !value.is_empty() {
                        blocks.push(message(role, value));
                    }
                }
            }
        }
    }
    if !matches!(format, Format::Messages) || value["content"].is_string() {
        let value = value_text(&value["content"]);
        if !value.is_empty() {
            blocks.push(message(role, value));
        }
    }
    for call in array(&value["tool_calls"]) {
        blocks.push(tool(
            "tool_call",
            &call["function"],
            &call["id"],
            &call["function"]["arguments"],
        ));
    }
    blocks
}

fn bounded(value: &str, remaining: &mut usize, truncated: &mut bool) -> String {
    let length = value.floor_char_boundary(value.len().min(*remaining));
    *remaining -= length;
    *truncated |= length < value.len();
    value[..length].into()
}

pub(super) struct Output {
    format: Format,
    stream: BTreeMap<(usize, usize), Block>,
    blocks: Vec<Block>,
    truncated: bool,
    complete: bool,
}

impl Output {
    pub(super) fn new(format: Format) -> Self {
        Self {
            format,
            stream: BTreeMap::new(),
            blocks: Vec::new(),
            truncated: false,
            complete: false,
        }
    }

    pub(super) fn view(&self) -> Value {
        let blocks: Vec<_> = if self.blocks.is_empty() {
            self.stream.values().collect()
        } else {
            self.blocks.iter().collect()
        };
        serde_json::json!({"blocks":blocks,"truncated":self.truncated,"complete":self.complete})
    }

    pub(super) fn finish(&mut self, complete: bool) {
        self.complete = complete;
    }

    pub(super) fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    pub(super) fn read_json(&mut self, value: &Value) {
        let mut blocks: Vec<_> = match self.format {
            Format::Responses => array(&value["output"])
                .iter()
                .flat_map(response_item)
                .collect(),
            Format::ChatCompletions => array(&value["choices"])
                .iter()
                .flat_map(|choice| conversation_message(self.format, &choice["message"]))
                .collect(),
            Format::Messages => conversation_message(self.format, value),
        };
        if let Some(error) = text(&value["error"]["message"]) {
            blocks.push(Block {
                kind: "error",
                text: error.into(),
                ..Block::default()
            });
        }
        if !blocks.is_empty() {
            let mut remaining = MAX_TEXT;
            self.truncated = blocks.len() > MAX_BLOCKS;
            self.blocks = blocks
                .into_iter()
                .take(MAX_BLOCKS)
                .map(|mut block| {
                    block.text = bounded(&block.text, &mut remaining, &mut self.truncated);
                    block
                })
                .collect();
            self.stream.clear();
        }
    }

    fn append(&mut self, key: (usize, usize), initial: Block, delta: &str, replace: bool) {
        if !self.stream.contains_key(&key) && self.stream.len() >= MAX_BLOCKS {
            self.truncated = true;
            return;
        }
        let mut remaining = MAX_TEXT.saturating_sub(
            self.stream
                .values()
                .map(|block| block.text.len())
                .sum::<usize>(),
        );
        let block = self.stream.entry(key).or_insert(initial);
        if replace {
            remaining += block.text.len();
            block.text.clear();
        }
        block
            .text
            .push_str(&bounded(delta, &mut remaining, &mut self.truncated));
    }

    fn put(&mut self, key: (usize, usize), mut block: Block) {
        let text = std::mem::take(&mut block.text);
        self.stream.remove(&key);
        self.append(key, block, &text, true);
    }

    pub(super) fn read_event(&mut self, event: &Value) {
        let index = |key: &str| event[key].as_u64().unwrap_or_default() as usize;
        match self.format {
            Format::Responses => {
                let output = index("output_index");
                match event["type"].as_str() {
                    Some("response.completed" | "response.incomplete" | "response.failed") => {
                        self.read_json(&event["response"])
                    }
                    Some("response.output_item.added" | "response.output_item.done") => {
                        let blocks = response_item(&event["item"]);
                        if event["type"] == "response.output_item.done" && !blocks.is_empty() {
                            self.stream.retain(|(item, _), _| *item != output);
                        }
                        for (part, block) in blocks.into_iter().enumerate() {
                            if !block.text.is_empty() || !self.stream.contains_key(&(output, part))
                            {
                                self.put((output, part), block);
                            }
                        }
                    }
                    Some("response.output_text.delta" | "response.refusal.delta") => {
                        self.append(
                            (output, index("content_index")),
                            message("assistant", String::new()),
                            text(&event["delta"]).unwrap_or_default(),
                            false,
                        );
                    }
                    Some("response.output_text.done" | "response.refusal.done") => {
                        if let Some(text) = text(&event["text"]).or_else(|| text(&event["refusal"]))
                        {
                            self.append(
                                (output, index("content_index")),
                                message("assistant", String::new()),
                                text,
                                true,
                            );
                        }
                    }
                    Some(
                        "response.function_call_arguments.delta"
                        | "response.custom_tool_call_input.delta",
                    ) => {
                        self.append(
                            (output, 0),
                            tool("tool_call", event, &event["item_id"], &Value::Null),
                            text(&event["delta"]).unwrap_or_default(),
                            false,
                        );
                    }
                    Some(
                        "response.function_call_arguments.done"
                        | "response.custom_tool_call_input.done",
                    ) => {
                        if let Some(arguments) =
                            text(&event["arguments"]).or_else(|| text(&event["input"]))
                        {
                            self.append(
                                (output, 0),
                                tool("tool_call", event, &event["item_id"], &Value::Null),
                                arguments,
                                true,
                            );
                        }
                    }
                    _ => {}
                }
            }
            Format::ChatCompletions => {
                for choice in array(&event["choices"]) {
                    let choice_index = choice["index"].as_u64().unwrap_or_default() as usize;
                    let delta = &choice["delta"];
                    if let Some(content) =
                        text(&delta["content"]).or_else(|| text(&delta["refusal"]))
                    {
                        self.append(
                            (choice_index, 0),
                            message("assistant", String::new()),
                            content,
                            false,
                        );
                    }
                    for call in array(&delta["tool_calls"]) {
                        let part =
                            (call["index"].as_u64().unwrap_or_default() as usize).saturating_add(1);
                        self.append(
                            (choice_index, part),
                            tool("tool_call", &call["function"], &call["id"], &Value::Null),
                            text(&call["function"]["arguments"]).unwrap_or_default(),
                            false,
                        );
                    }
                }
            }
            Format::Messages => {
                let part = index("index");
                match event["type"].as_str() {
                    Some("content_block_start") => {
                        let block = &event["content_block"];
                        let value = serde_json::json!({"role":"assistant","content":[block]});
                        if let Some(mut value) =
                            conversation_message(self.format, &value).into_iter().next()
                        {
                            if value.kind == "tool_call"
                                && block["input"].as_object().is_some_and(Map::is_empty)
                            {
                                value.text.clear();
                            }
                            self.put((part, 0), value);
                        }
                    }
                    Some("content_block_delta") => match event["delta"]["type"].as_str() {
                        Some("text_delta") => self.append(
                            (part, 0),
                            message("assistant", String::new()),
                            text(&event["delta"]["text"]).unwrap_or_default(),
                            false,
                        ),
                        Some("input_json_delta") => self.append(
                            (part, 0),
                            tool("tool_call", &Value::Null, &Value::Null, &Value::Null),
                            text(&event["delta"]["partial_json"]).unwrap_or_default(),
                            false,
                        ),
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }
}
