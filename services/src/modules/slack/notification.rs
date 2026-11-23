use std::sync::LazyLock;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::{Captures, Regex};
use unicode_segmentation::UnicodeSegmentation;
use url::Url;

use super::{
    mailbox::UserDirectory,
    protocol::{BlockText, Message, MessageBlock, valid_id},
};

const MAX_BODY_CHARS: usize = 240;

pub(super) fn body(message: &Message, users: Option<&UserDirectory>) -> String {
    let mut parts = Vec::new();
    let mut has_prose = false;
    let mut has_table = false;
    for block in &message.blocks {
        match block {
            MessageBlock::Section { text, fields } => {
                let render = |part: &BlockText| match part {
                    BlockText::PlainText { text } => slack_entities(text),
                    BlockText::Mrkdwn { text } => slack_text(text, users),
                };
                let fields = fields.iter().map(|field| single_line(&render(field)));
                for text in text.iter().map(render).chain(fields) {
                    if !text.trim().is_empty() {
                        parts.push(text);
                        has_prose = true;
                    }
                }
            }
            MessageBlock::Markdown { text } => {
                let text = markdown_text(text, false);
                if !text.trim().is_empty() {
                    parts.push(text);
                    has_prose = true;
                }
            }
            MessageBlock::RichTextPreformatted { text, .. } => {
                let mut lines = text.lines().filter(|line| !line.trim().is_empty());
                if let Some(first) = lines.next() {
                    let suffix = if lines.next().is_some() { " …" } else { "" };
                    parts.push(format!("{}{suffix}", slack_entities(first)));
                }
            }
            MessageBlock::Table { rows } => {
                let rows = table_text(rows);
                has_table |= !rows.is_empty();
                parts.extend(rows);
            }
        }
    }
    if !has_table {
        parts.extend(message.tables.iter().flat_map(|rows| table_text(rows)));
    }
    if !has_prose {
        let text = slack_text(&message.text, users);
        if !text.trim().is_empty() {
            parts.insert(0, text);
        }
    }
    if parts.is_empty() {
        let attachment = message
            .attachments
            .first()
            .map(|item| item.title.as_str())
            .filter(|title| !title.trim().is_empty())
            .or(message.attachment.as_deref())
            .filter(|title| !title.trim().is_empty());
        parts.push(attachment.map_or_else(|| "点击查看消息".into(), slack_entities));
    }

    let mut summary = String::new();
    for line in parts.iter().flat_map(|part| part.lines()) {
        let line = line.trim_end();
        if line.is_empty() {
            if !summary.is_empty() && !summary.ends_with("\n\n") {
                summary.push('\n');
            }
        } else {
            if !summary.is_empty() {
                summary.push('\n');
            }
            summary.push_str(line);
        }
    }
    let summary = summary.trim();
    let output = if let Some(author) = &message.author {
        let name = author.name.trim();
        let name = if name.is_empty() || name == author.id {
            display_name(users, &author.id)
        } else {
            name
        };
        format!("{name} · {summary}")
    } else {
        summary.to_owned()
    };
    truncate(output)
}

fn table_text(rows: &[Vec<String>]) -> Vec<String> {
    let mut rows = rows
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()));
    let mut result: Vec<_> = rows
        .by_ref()
        .take(2)
        .map(|row| {
            let mut cells: Vec<_> = row
                .iter()
                .take(3)
                .map(|cell| single_line(&slack_entities(cell)))
                .collect();
            if row.len() > 3 {
                cells.push("…".into());
            }
            cells.join(" · ")
        })
        .collect();
    if rows.next().is_some() {
        result.push("…".into());
    }
    result
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn display_name<'a>(users: Option<&'a UserDirectory>, id: &'a str) -> &'a str {
    users
        .and_then(|directory| directory.find(id))
        .map(|user| user.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(id)
}

fn slack_entities(text: &str) -> String {
    static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(amp|lt|gt);").unwrap());
    ENTITY
        .replace_all(text, |captures: &Captures<'_>| match &captures[1] {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            _ => unreachable!(),
        })
        .into_owned()
}

fn slack_text(text: &str, users: Option<&UserDirectory>) -> String {
    // 先识别 Slack 链接和提及，保护代码中的字面内容。
    static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)(?P<code>```(?s:.*?)```|``[^`\n]+``|`[^`\n]+`)|(?P<quote>^[ \t]*&gt;[ \t]?)|<(?P<reference>[^<>\n]+)>")
            .unwrap()
    });
    let markdown = TOKEN.replace_all(text, |captures: &Captures<'_>| {
        if let Some(code) = captures.name("code") {
            return code.as_str().to_owned();
        }
        if captures.name("quote").is_some() {
            return "> ".into();
        }
        let reference = &captures["reference"];
        let (target, label) = reference
            .split_once('|')
            .map_or((reference, None), |(target, label)| (target, Some(label)));
        let label = label.filter(|label| !label.trim().is_empty());
        let visible = match target.as_bytes().first() {
            Some(b'@') if valid_id(&target[1..]) => {
                format!("@{}", display_name(users, &target[1..]))
            }
            Some(b'#') if valid_id(&target[1..]) => {
                let name = label.unwrap_or(&target[1..]);
                format!("#{}", name.trim_start_matches('#'))
            }
            Some(b'!') => match &target[1..] {
                "channel" | "here" | "everyone" => format!("@{}", &target[1..]),
                _ => label.unwrap_or(reference).to_owned(),
            },
            _ => match Url::parse(target) {
                Ok(url) => label.or(url.host_str()).unwrap_or(target).to_owned(),
                Err(_) => format!("<{reference}>"),
            },
        };
        let mut escaped = String::new();
        for character in slack_entities(&visible).chars() {
            if character.is_ascii_punctuation() {
                escaped.push('\\');
            }
            escaped.push(character);
        }
        escaped
    });
    markdown_text(&markdown, true)
}

fn markdown_text(text: &str, decode_slack_code: bool) -> String {
    let mut output = String::new();
    let mut in_code_block = false;
    for event in Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH) {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                if !output.ends_with('\n') {
                    output.push('\n');
                }
            }
            Event::Text(text) if in_code_block && decode_slack_code => {
                output.push_str(&slack_entities(&text));
            }
            Event::Code(text) if decode_slack_code => output.push_str(&slack_entities(&text)),
            Event::Text(text) | Event::Code(text) => output.push_str(&text),
            Event::SoftBreak
            | Event::HardBreak
            | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                if !output.ends_with('\n') {
                    output.push('\n');
                }
            }
            _ => {}
        }
    }
    output
}

fn truncate(mut text: String) -> String {
    if text.chars().count() <= MAX_BODY_CHARS {
        return text;
    }
    let mut end = 0;
    let mut chars = 0;
    for grapheme in text.graphemes(true) {
        chars += grapheme.chars().count();
        if chars >= MAX_BODY_CHARS {
            break;
        }
        end += grapheme.len();
    }
    text.truncate(text[..end].trim_end().len());
    text.push('…');
    text
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn message(fields: Value) -> Message {
        let mut fields = fields.as_object().unwrap().clone();
        fields.insert("ts".into(), json!("1.000001"));
        Message::from_slack(&Value::Object(fields), None).unwrap()
    }

    #[test]
    fn renders_sections_and_markdown_in_order() {
        let message = message(json!({
            "user": "UAUTHOR",
            "user_profile": {"display_name": "值班机器人"},
            "text": "顶层回退",
            "blocks": [
                {"type": "section",
                 "text": {"type": "mrkdwn", "text": "&gt;值班 <@U123>: *<https://example.com/alerts|IP绑定不通>*\n&gt;已持续0.4小时: `有3个绑定的IP无法连通`"},
                 "fields": [
                     {"type": "plain_text", "text": "*按字面显示*\n&amp;lt;"},
                     {"type": "mrkdwn", "text": "<#C123|告警> <https://example.net/path> ~过期~ `<@U123>`"}
                 ]},
                {"type": "actions"},
                {"type": "markdown", "text": "## [执行结果](https://example.org)\n\n- **成功**\n\n```json\n{}\n```"}
            ]
        }));
        assert_eq!(
            body(&message, None),
            "值班机器人 · 值班 @U123: IP绑定不通\n已持续0.4小时: 有3个绑定的IP无法连通\n*按字面显示* &lt;\n#告警 example.net 过期 <@U123>\n执行结果\n成功\n{}"
        );
    }

    #[test]
    fn includes_top_level_text_with_tables_and_preformatted_blocks() {
        let mut message = message(json!({
            "text": "运行结果",
            "blocks": [
                {"type": "table", "rows": [
                    [{"type":"raw_text","text":"环境"},{"type":"raw_text","text":"状态"}],
                    [{"type":"raw_text","text":"生产"},{"type":"raw_text","text":"成功"},
                     {"type":"raw_text","text":"23:15"},{"type":"raw_text","text":"详情"}],
                    [{"type":"raw_text","text":"测试"}]
                ]},
                {"type": "rich_text", "elements": [
                    {"type":"rich_text_preformatted",
                     "elements":[{"type":"text","text":"\n*第一行*\n第二行\n"}]}
                ]}
            ]
        }));
        let table = "运行结果\n环境 · 状态\n生产 · 成功 · 23:15 · …\n…";
        assert_eq!(body(&message, None), format!("{table}\n*第一行* …"));
        message.blocks.clear();
        assert_eq!(body(&message, None), table);
    }

    #[test]
    fn falls_back_to_attachments_and_truncates_by_grapheme() {
        let mut attachment = message(json!({
            "blocks": [{"type":"unknown"}],
            "files": [{"name":"截图.png"}]
        }));
        assert_eq!(body(&attachment, None), "截图.png");
        attachment.attachments.clear();
        assert_eq!(body(&attachment, None), "附件 · 截图.png");
        attachment.attachment = None;
        assert_eq!(body(&attachment, None), "点击查看消息");

        let prefix = "甲".repeat(238);
        let long = message(json!({"text": format!("{prefix}👩‍💻乙")}));
        assert_eq!(body(&long, None), format!("{prefix}…"));
    }
}
