use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::protocol::Protocol;

#[derive(Clone, Copy, Deserialize, Serialize)]
pub(super) enum Format {
    Responses,
    ChatCompletions,
    Messages,
}

impl Format {
    pub(super) fn for_endpoint(protocol: Protocol, endpoint: &str) -> Option<Self> {
        match (protocol, endpoint) {
            (Protocol::Codex, "/responses") => Some(Self::Responses),
            (Protocol::Codex, "/chat/completions") => Some(Self::ChatCompletions),
            (Protocol::Claude, "/messages") => Some(Self::Messages),
            _ => None,
        }
    }

    pub(super) fn read(self, request: &Value) -> Vec<Instruction> {
        let mut instructions = Vec::new();
        match self {
            Self::Responses => {
                Instruction::push(
                    &mut instructions,
                    "instructions",
                    None,
                    &request["instructions"],
                );
                read_messages(&mut instructions, "input", &request["input"]);
            }
            Self::ChatCompletions => {
                read_messages(&mut instructions, "messages", &request["messages"]);
            }
            Self::Messages => {
                let system = &request["system"];
                if let Some(blocks) = system.as_array() {
                    for (index, block) in blocks.iter().enumerate() {
                        if block["type"] == "text" {
                            Instruction::push(
                                &mut instructions,
                                format!("system[{index}].text"),
                                Some("system"),
                                &block["text"],
                            );
                        }
                    }
                } else {
                    Instruction::push(&mut instructions, "system", Some("system"), system);
                }
            }
        }
        instructions
    }
}

#[derive(Serialize)]
pub(super) struct Instruction {
    source: String,
    role: Option<&'static str>,
    text: String,
}

impl Instruction {
    fn push(
        instructions: &mut Vec<Self>,
        source: impl Into<String>,
        role: Option<&'static str>,
        value: &Value,
    ) {
        if let Some(text) = value.as_str().filter(|text| !text.is_empty()) {
            instructions.push(Self {
                source: source.into(),
                role,
                text: text.to_owned(),
            });
        }
    }
}

fn read_messages(instructions: &mut Vec<Instruction>, field: &str, value: &Value) {
    let Some(messages) = value.as_array() else {
        return;
    };
    for (index, message) in messages.iter().enumerate() {
        let role = match message["role"].as_str() {
            Some("system") => Some("system"),
            Some("developer") => Some("developer"),
            _ => continue,
        };
        let content = &message["content"];
        if let Some(parts) = content.as_array() {
            for (part, value) in parts.iter().enumerate() {
                if matches!(value["type"].as_str(), Some("text" | "input_text")) {
                    Instruction::push(
                        instructions,
                        format!("{field}[{index}].content[{part}].text"),
                        role,
                        &value["text"],
                    );
                }
            }
        } else {
            Instruction::push(instructions, format!("{field}[{index}]"), role, content);
        }
    }
}

pub(super) enum Capture {
    Captured(Vec<Instruction>),
    Unavailable,
}

impl Capture {
    pub(super) fn view(&self) -> Value {
        match self {
            Self::Captured(blocks) if blocks.is_empty() => {
                json!({"status":"absent","count":0})
            }
            Self::Captured(blocks) => {
                json!({"status":"captured","count":blocks.len(),"blocks":blocks})
            }
            Self::Unavailable => json!({"status":"unavailable"}),
        }
    }
}
