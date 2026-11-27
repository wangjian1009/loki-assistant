use orgize::Org;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd, html};

pub fn org(fragment: &str) -> String {
    sanitize(&Org::parse(fragment).to_html())
}

pub fn markdown(fragment: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut events = Parser::new_ext(fragment, options);
    let mut rendered = String::new();
    html::push_html(
        &mut rendered,
        std::iter::from_fn(|| {
            let event = events.next()?;
            if matches!(&event, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
            if matches!(info.split_whitespace().next(), Some("org" | "org-mode")))
            {
                let mut source = String::new();
                for code in events.by_ref() {
                    match code {
                        Event::End(TagEnd::CodeBlock) => break,
                        Event::Text(text) => source.push_str(&text),
                        _ => {}
                    }
                }
                Some(Event::Html(Org::parse(source).to_html().into()))
            } else {
                Some(match event {
                    Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
                    other => other,
                })
            }
        }),
    );
    sanitize(&rendered)
}

pub fn render(fragment: &str) -> String {
    if looks_like_org(fragment) {
        org(fragment)
    } else {
        markdown(fragment)
    }
}

fn looks_like_org(fragment: &str) -> bool {
    let mut fence: Option<(u8, usize)> = None;
    let mut org_marker = false;
    let mut single_star_heading = false;
    let mut org_list = false;
    for line in fragment.lines() {
        let trimmed = line.trim_start();
        let first = trimmed.as_bytes().first().copied();
        let marker = first.filter(|m| matches!(m, b'`' | b'~'));
        let count = marker.map_or(0, |m| trimmed.bytes().take_while(|b| *b == m).count());
        if let Some((open, length)) = fence {
            if marker == Some(open) && count >= length && trimmed[count..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if let Some(mark) = marker.filter(|_| count >= 3) {
            fence = Some((mark, count));
            continue;
        }
        if trimmed.starts_with("# ") || trimmed.starts_with("##") {
            return false;
        }
        org_marker |= trimmed.starts_with("#+") || trimmed.contains("[[") && trimmed.contains("]]");
        org_list |= trimmed.starts_with("- ") || trimmed.starts_with("+ ");
        let stars = line.bytes().take_while(|b| *b == b'*').count();
        if stars > 0 && line.as_bytes().get(stars) == Some(&b' ') {
            org_marker |= stars > 1 || line[stars + 1..].starts_with("TODO ");
            single_star_heading |= stars == 1;
        }
    }
    org_marker || single_star_heading && org_list
}

fn sanitize(html: &str) -> String {
    // 会话内容来自远端。保留结构与代码语言，禁止原始脚本、外部图片加载和危险链接。
    let clean = ammonia::Builder::default()
        .add_tags(["input"])
        .add_url_schemes(["file"])
        .attribute_filter(|tag, attr, value| {
            let inline = tag == "img" && attr == "src";
            let link = tag == "a"
                && attr == "href"
                && value.split(['?', '#']).next().is_some_and(|path| {
                    let path = path.to_ascii_lowercase();
                    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
                        .iter()
                        .any(|ext| path.ends_with(ext))
                });
            if inline || link {
                if value.len() > 4096
                    || value.chars().any(char::is_control)
                    || value.contains("://")
                        && !["https://", "http://", "file://"]
                            .iter()
                            .any(|prefix| value.starts_with(prefix))
                    || value.starts_with("data:")
                {
                    return None;
                }
                let encoded = value
                    .as_bytes()
                    .iter()
                    .map(|b| format!("%{b:02X}"))
                    .collect::<String>();
                let mode = if inline { "inline" } else { "open" };
                return Some(format!("https://loki-image.invalid/{mode}/{encoded}").into());
            }
            if attr == "href" && value.starts_with("file:") {
                return None;
            }
            Some(value.into())
        })
        .add_tag_attributes("code", ["class"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .clean(html)
        .to_string();
    // 图片由本地组件拦截加载，整张图片也是查看原图的入口。
    let mut output = String::new();
    let mut remainder = clean.as_str();
    while let Some(start) = remainder.find("<img ") {
        output.push_str(&remainder[..start]);
        let image = &remainder[start..];
        let Some(end) = image.find('>') else { break };
        let tag = &image[..=end];
        if let Some(source) = tag
            .split_once("src=\"https://loki-image.invalid/inline/")
            .and_then(|(_, src)| src.split('"').next())
        {
            output.push_str("<a href=\"https://loki-image.invalid/open/");
            output.push_str(source);
            output.push_str("\">");
            output.push_str(tag);
            output.push_str("</a>");
        } else {
            output.push_str(tag);
        }
        remainder = &image[end + 1..];
    }
    output.push_str(remainder);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_lists_and_source_blocks_from_both_formats() {
        let org = org("* Heading\n- item\n\n#+begin_src rust\n  fn main() {}\n#+end_src\n");
        let md = markdown("# Heading\n- item\n\n```rust\n  fn main() {}\n```\n");
        for html in [org, md] {
            assert!(html.contains("<h1>Heading</h1>"), "{html}");
            assert!(html.contains("<ul>") && html.contains("<li>"), "{html}");
            assert!(
                html.contains("<pre><code class=\"language-rust\">  fn main() {}"),
                "{html}"
            );
        }
    }

    #[test]
    fn embedded_org_and_untrusted_markup() {
        let html = render(
            "# Markdown\n\n```org\n* Org\n#+begin_src sh\n  echo x\n#+end_src\n```\n<script>bad()</script>\n",
        );
        assert!(
            html.contains("<h1>Markdown</h1>") && html.contains("<h1>Org</h1>"),
            "{html}"
        );
        assert!(
            html.contains("language-sh") && html.contains("  echo x"),
            "{html}"
        );
        assert!(!html.contains("<script>"));
        assert!(
            !org("@@html:<img src='http://example.org'>@@\n").contains("src=\"http://example.org")
        );
        assert!(
            markdown("![图](image.png) [大图](image.png)")
                .contains("https://loki-image.invalid/inline/")
        );
        assert!(markdown("[大图](image.png)").contains("https://loki-image.invalid/open/"));
        assert!(render("* 标题\n- 列表").contains("<h1>标题</h1>"));
        assert!(render("* 甲\n* 乙").contains("<li>甲</li>"));
        assert!(
            markdown("- 外层\n\n  ```org\n  * 标题\n  ```\n\n  后文")
                .contains("</h1>\n<p>后文</p>\n</li>")
        );
    }
}
