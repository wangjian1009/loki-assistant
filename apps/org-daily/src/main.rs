mod dates;
mod org;
mod query;

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Parser, ValueEnum};
use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::org::{ConfigReader, Heading, Sections};
use crate::query::{Expression, PropertyFilter};

const QUERY_HELP: &str = r#"筛选规则
  --from 和 --to 按日记文件名中的日期筛选，包含首尾两天。省略哪一端，哪一端就不限。
  --include-undated 也查询未以日期命名的 .org 文件，只能用于无日期范围的查询。
  默认查询所有标题。--tasks 只查带任务状态的标题，包含已完成任务。
  --state 可重复，匹配其中任一状态，区分大小写。状态默认有 TODO、DONE，
  也可由日记或本地 SETUPFILE 中的 TODO、SEQ_TODO、TYP_TODO 指令定义。
  --title 匹配标题的 Org 源文本。--body 匹配标题的直属正文，子标题单独查询。
  两者使用 Rust 正则表达式。-F 按字面匹配，-i 忽略大小写。
  日期、任务状态、标题、正文、标签、属性和 --where 条件须同时满足。
  重复的 --tag、--local-tag 和 --property 也须逐项满足。

标签和属性
  --tag TAG 精确匹配标签，区分大小写。标签包括当前标题、父标题以及日记和
  本地 SETUPFILE 中的 FILETAGS。
  --local-tag TAG 仅匹配当前标题显式写出的标签，区分大小写。
  --property KEY 检查属性是否存在，--property KEY=VALUE 精确匹配属性值，
  --property KEY= 匹配存在且值为空的属性。属性取自当前标题的 PROPERTIES drawer。
  属性名忽略 ASCII 大小写，属性值区分大小写。

--where 表达式
  tag:TAG          匹配标签，规则同 --tag TAG
  prop:KEY         检查属性是否存在
  prop:KEY=VALUE   精确匹配属性值
  prop:KEY=""      匹配存在且值为空的属性
  NOT prop:KEY     匹配缺少该属性的标题

  优先级从高到低为 NOT、AND、OR。用括号分组，括号和 NOT 可嵌套至 64 层。
  tag、prop 和布尔运算符忽略大小写。条件之间须写明 AND 或 OR。
  值含空白、冒号、等号、括号或双引号时，用双引号括起，并按 JSON 字符串规则
  转义。例如 prop:NOTE="他说 \"可以\""。
  在 shell 中可用单引号包住整个表达式。直接传参数数组时，把表达式作为
  --where 后的一个参数，省略 shell 单引号。语法错误会指明字符位置。

示例
  org-daily --from 2026-11-01 --to 2026-11-26 --title '^(阅读|复盘)' --where '(tag:工作 OR tag:阅读) AND prop:ID AND NOT prop:STATUS=archived'
  org-daily --tasks --state TODO --state WAITING --tag 工作 --property 'AREA=product infrastructure' --format jsonl
  org-daily --body '复盘|结论' --where 'prop:NOTE="" OR NOT prop:NOTE' --format summary

输出与退出码
  文件按日期倒序，同一文件按标题顺序输出。--format jsonl 每条记录输出一个 JSON 对象，
  字段为 date、file、line、level、title、outline、state、tags、properties、body。
  无日期的文件排在最后，其 JSON date 字段为 null。
  退出码 0 表示命中，1 表示无匹配，2 表示参数错误或读取失败。"#;

#[derive(Parser)]
#[command(
    version,
    about,
    after_help = "完整筛选语法和查询示例见 org-daily --help。",
    after_long_help = QUERY_HELP
)]
struct Args {
    /// Org-roam 日记目录，默认使用 ~/workspace/org-roam/daily
    #[arg(long, env = "ORG_ROAM_DAILIES_DIR", value_name = "DIR")]
    dir: Option<PathBuf>,

    /// 起始日期，包含当天
    #[arg(long, value_parser = dates::parse_date, value_name = "YYYY-MM-DD")]
    from: Option<NaiveDate>,

    /// 截止日期，包含当天
    #[arg(long, value_parser = dates::parse_date, value_name = "YYYY-MM-DD")]
    to: Option<NaiveDate>,

    /// 同时查询非日期命名的 .org 文件
    #[arg(long, conflicts_with_all = ["from", "to"])]
    include_undated: bool,

    /// 标题正则表达式
    #[arg(long, value_name = "PATTERN")]
    title: Option<String>,

    /// 标题直属正文的正则表达式
    #[arg(long, value_name = "PATTERN")]
    body: Option<String>,

    /// 按字面匹配标题和正文
    #[arg(short = 'F', long)]
    fixed_strings: bool,

    /// 标题和正文匹配时忽略大小写
    #[arg(short, long)]
    ignore_case: bool,

    /// 要求存在该标签，可重复
    #[arg(long, value_name = "TAG", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    tag: Vec<String>,

    /// 要求标签直接写在当前标题上，可重复
    #[arg(long, value_name = "TAG", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    local_tag: Vec<String>,

    /// 按属性存在或精确值筛选，可重复
    #[arg(long, value_name = "KEY[=VALUE]", value_parser = PropertyFilter::parse)]
    property: Vec<PropertyFilter>,

    /// 标签和属性表达式，支持 AND、OR、NOT 和括号
    #[arg(long, value_name = "EXPR", value_parser = Expression::parse)]
    r#where: Option<Expression>,

    /// 仅显示带任务状态的标题
    #[arg(long)]
    tasks: bool,

    /// 允许的任务状态，可重复
    #[arg(long, value_name = "STATE", value_parser = clap::builder::NonEmptyStringValueParser::new())]
    state: Vec<String>,

    /// 输出格式
    #[arg(long, value_enum, default_value_t = Format::Text)]
    format: Format,
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    /// 每条记录一行，带文件位置
    Text,
    /// 每条记录一个 JSON 对象，包含正文和元数据
    Jsonl,
    /// 按日期、任务状态和标签统计命中数
    Summary,
}

#[derive(Serialize)]
struct Entry<'a> {
    date: Option<String>,
    file: Cow<'a, str>,
    line: usize,
    level: usize,
    title: &'a str,
    outline: Vec<&'a str>,
    state: Option<&'a str>,
    tags: BTreeSet<&'a str>,
    properties: BTreeMap<String, String>,
    body: &'a str,
}

impl Entry<'_> {
    fn write(&self, output: &mut impl Write, format: Format) -> Result<()> {
        match format {
            Format::Text => {
                write!(
                    output,
                    "{}:{}:\t{}\t",
                    single_line(&self.file),
                    self.line,
                    self.date.as_deref().unwrap_or("-")
                )?;
                if let Some(state) = self.state {
                    write!(output, "{} ", single_line(state))?;
                }
                write!(output, "{}", single_line(self.title))?;
                if !self.tags.is_empty() {
                    write!(output, "\t:")?;
                    for tag in &self.tags {
                        write!(output, "{}:", single_line(tag))?;
                    }
                }
                writeln!(output)?;
            }
            Format::Jsonl => {
                serde_json::to_writer(&mut *output, self)?;
                writeln!(output)?;
            }
            Format::Summary => unreachable!("汇总输出由 Summary 处理"),
        }
        output.flush()?;
        Ok(())
    }
}

fn single_line(text: &str) -> Cow<'_, str> {
    if !text.contains(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_control() {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    Cow::Owned(escaped)
}

#[derive(Default)]
struct Summary {
    total: usize,
    dates: BTreeMap<Option<String>, usize>,
    states: BTreeMap<String, usize>,
    tags: BTreeMap<String, usize>,
}

impl Summary {
    fn add(&mut self, entry: &Entry<'_>) {
        self.total += 1;
        *self.dates.entry(entry.date.clone()).or_default() += 1;
        *self
            .states
            .entry(entry.state.unwrap_or("无状态").to_owned())
            .or_default() += 1;
        for &tag in &entry.tags {
            *self.tags.entry(tag.to_owned()).or_default() += 1;
        }
    }

    fn write(self, output: &mut impl Write) -> io::Result<()> {
        writeln!(output, "记录数\t{}", self.total)?;
        for (date, count) in self.dates.into_iter().rev() {
            writeln!(
                output,
                "日期\t{}\t{count}",
                date.as_deref().unwrap_or("无日期")
            )?;
        }
        for (state, count) in self.states {
            writeln!(output, "状态\t{}\t{count}", single_line(&state))?;
        }
        for (tag, count) in self.tags {
            writeln!(output, "标签\t{}\t{count}", single_line(&tag))?;
        }
        output.flush()
    }
}

fn default_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("请用 --dir 指定日记目录")?;
    Ok(PathBuf::from(home).join("workspace/org-roam/daily"))
}

fn pattern(args: &Args, value: Option<&str>, flag: &str) -> Result<Option<Regex>> {
    value
        .map(|value| {
            let pattern = if args.fixed_strings {
                Cow::Owned(regex::escape(value))
            } else {
                Cow::Borrowed(value)
            };
            RegexBuilder::new(&pattern)
                .case_insensitive(args.ignore_case)
                .multi_line(true)
                .build()
                .with_context(|| format!("{flag} 正则表达式无效"))
        })
        .transpose()
}

fn search(args: &Args, output: &mut impl Write) -> Result<bool> {
    let title_pattern = pattern(args, args.title.as_deref(), "--title")?;
    let body_pattern = pattern(args, args.body.as_deref(), "--body")?;
    let collect_body = body_pattern.is_some() || matches!(args.format, Format::Jsonl);
    let root = match args.dir.as_deref() {
        Some(dir) => dir.to_path_buf(),
        None => default_dir()?,
    };
    let candidates = dates::candidates(&root, args.from, args.to, args.include_undated)?;
    let mut configs = ConfigReader::new();
    let mut summary = Summary::default();
    let mut found = false;
    for (date, path) in candidates {
        let source = org::read_source(&path)?;
        let mut sections = Sections::new(&source);
        let config = configs.read(&path, sections.preamble())?;
        let mut parents: Vec<Heading<'_>> = Vec::new();
        while let Some((line, level, raw, section)) = sections.next(collect_body) {
            let heading = Heading::parse(level, raw, &config);
            while parents.last().is_some_and(|parent| parent.level >= level) {
                parents.pop();
            }
            let matches = (!args.tasks || heading.state.is_some())
                && (args.state.is_empty()
                    || heading
                        .state
                        .is_some_and(|state| args.state.iter().any(|value| value == state)))
                && title_pattern
                    .as_ref()
                    .is_none_or(|pattern| pattern.is_match(heading.title))
                && args
                    .local_tag
                    .iter()
                    .all(|tag| heading.tags.contains(&tag.as_str()))
                && args
                    .property
                    .iter()
                    .all(|filter| filter.matches(&section.properties));

            if matches {
                let tags: BTreeSet<&str> = config
                    .tags
                    .iter()
                    .map(String::as_str)
                    .chain(
                        parents
                            .iter()
                            .chain(std::iter::once(&heading))
                            .flat_map(|item| item.tags.iter().copied()),
                    )
                    .collect();
                if args.tag.iter().all(|tag| tags.contains(tag.as_str()))
                    && args
                        .r#where
                        .as_ref()
                        .is_none_or(|expression| expression.matches(&tags, &section.properties))
                {
                    let body = section.body.join("\n");
                    let body = body.trim_matches('\n');
                    if body_pattern
                        .as_ref()
                        .is_none_or(|pattern| pattern.is_match(body))
                    {
                        let entry = Entry {
                            date: date.map(|date| date.to_string()),
                            file: path.to_string_lossy(),
                            line,
                            level,
                            title: heading.title,
                            outline: parents.iter().map(|item| item.title).collect(),
                            state: heading.state,
                            tags,
                            properties: section.properties,
                            body,
                        };
                        match args.format {
                            Format::Summary => summary.add(&entry),
                            format => entry.write(output, format)?,
                        }
                        found = true;
                    }
                }
            }
            parents.push(heading);
        }
    }
    if matches!(args.format, Format::Summary) {
        summary.write(output)?;
    }
    Ok(found)
}

fn main() -> ExitCode {
    let args = Args::parse();
    match search(&args, &mut io::stdout().lock()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            let broken_pipe = error.chain().any(|cause| {
                cause
                    .downcast_ref::<io::Error>()
                    .is_some_and(|err| err.kind() == io::ErrorKind::BrokenPipe)
                    || cause
                        .downcast_ref::<serde_json::Error>()
                        .and_then(serde_json::Error::io_error_kind)
                        == Some(io::ErrorKind::BrokenPipe)
            });
            if broken_pipe {
                return ExitCode::SUCCESS;
            }
            eprintln!("org-daily: {error:#}");
            ExitCode::from(2)
        }
    }
}
