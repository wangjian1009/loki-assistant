use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::iter::Peekable;
use std::path::{Path, PathBuf};
use std::str::Lines;

use anyhow::{Context, Result};

#[derive(Default)]
pub struct Keywords {
    pub tags: Vec<String>,
    pub states: Vec<String>,
}

pub struct ConfigReader {
    setups: HashMap<PathBuf, Vec<(String, String)>>,
}

impl ConfigReader {
    pub fn new() -> Self {
        Self {
            setups: HashMap::new(),
        }
    }

    pub fn read(&mut self, path: &Path, section: Section<'_>) -> Result<Keywords> {
        let mut config = Keywords::default();
        let mut seen = HashSet::new();
        for (key, value) in section.keywords {
            self.apply(path, key, value, &mut config, &mut seen)?;
        }
        if config.states.is_empty() {
            config.states = vec!["TODO".into(), "DONE".into()];
        }
        Ok(config)
    }

    fn apply(
        &mut self,
        path: &Path,
        key: &str,
        value: &str,
        config: &mut Keywords,
        seen: &mut HashSet<PathBuf>,
    ) -> Result<()> {
        if key.eq_ignore_ascii_case("FILETAGS") {
            config.tags.extend(
                value
                    .split(|ch: char| ch == ':' || ch.is_whitespace())
                    .filter(|tag| !tag.is_empty())
                    .map(str::to_owned),
            );
        } else if ["TODO", "SEQ_TODO", "TYP_TODO"]
            .iter()
            .any(|name| key.eq_ignore_ascii_case(name))
        {
            config.states.extend(
                value
                    .split_whitespace()
                    .map(|word| word.split('(').next().unwrap_or_default())
                    .filter(|word| !word.is_empty() && *word != "|")
                    .map(str::to_owned),
            );
        } else if key.eq_ignore_ascii_case("SETUPFILE") {
            let target = value.trim_matches('"');
            if target.is_empty() || target.contains("://") {
                return Ok(());
            }
            let target = if let Some(relative) = target.strip_prefix("~/") {
                let Some(home) = std::env::var_os("HOME") else {
                    return Ok(());
                };
                PathBuf::from(home).join(relative)
            } else {
                path.with_file_name(target)
            };
            let target = match target.canonicalize() {
                Ok(target) => target,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("无法打开配置 {}", target.display()));
                }
            };
            if !seen.insert(target.clone()) {
                return Ok(());
            }
            if !self.setups.contains_key(&target) {
                let source = read_source(&target)?;
                let keywords = Sections::new(&source)
                    .preamble()
                    .keywords
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value.to_owned()))
                    .collect();
                self.setups.insert(target.clone(), keywords);
            }
            for (key, value) in self.setups[&target].clone() {
                self.apply(&target, &key, &value, config, seen)?;
            }
        }
        Ok(())
    }
}

pub fn read_source(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))
}

pub struct Heading<'a> {
    pub level: usize,
    pub title: &'a str,
    pub state: Option<&'a str>,
    pub tags: Vec<&'a str>,
}

impl<'a> Heading<'a> {
    pub fn parse(level: usize, raw: &'a str, config: &Keywords) -> Self {
        let (title, tags) = match raw.split_whitespace().next_back().and_then(tag_suffix) {
            Some(tags) => {
                let start = raw.rfind(tags).expect("标签来自原标题");
                (
                    raw[..start].trim_end(),
                    tags.split(':').filter(|tag| !tag.is_empty()).collect(),
                )
            }
            None => (raw, Vec::new()),
        };

        let first = title.split_whitespace().next().unwrap_or_default();
        let state = config
            .states
            .iter()
            .any(|state| state == first)
            .then_some(first);
        let title = if state.is_some() {
            title[first.len()..].trim_start()
        } else {
            title
        };
        let title = title
            .strip_prefix("[#")
            .and_then(|priority| priority.split_once(']'))
            .filter(|(priority, rest)| {
                priority.chars().count() == 1
                    && (rest.is_empty() || rest.starts_with(char::is_whitespace))
            })
            .map_or(title, |(_, rest)| rest.trim_start());

        Self {
            level,
            title,
            state,
            tags,
        }
    }
}

fn tag_suffix(word: &str) -> Option<&str> {
    let tags = word.strip_prefix(':')?.strip_suffix(':')?;
    (tags.chars().any(|ch| ch != ':')
        && tags
            .chars()
            .all(|ch| ch.is_alphanumeric() || "_@#%:".contains(ch)))
    .then_some(word)
}

#[derive(Default)]
pub struct Section<'a> {
    pub keywords: Vec<(&'a str, &'a str)>,
    pub properties: BTreeMap<String, String>,
    pub body: Vec<&'a str>,
}

pub struct Sections<'a> {
    lines: Peekable<std::iter::Enumerate<Lines<'a>>>,
}

impl<'a> Sections<'a> {
    pub fn new(source: &'a str) -> Self {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        Self {
            lines: source.lines().enumerate().peekable(),
        }
    }

    pub fn preamble(&mut self) -> Section<'a> {
        self.section(true, false)
    }

    pub fn next(&mut self, collect_body: bool) -> Option<(usize, usize, &'a str, Section<'a>)> {
        let (number, line) = self.lines.next()?;
        let (level, title) = headline(line).expect("上一节停在标题行");
        Some((number + 1, level, title, self.section(false, collect_body)))
    }

    fn section(&mut self, preamble: bool, collect_body: bool) -> Section<'a> {
        let mut section = Section::default();
        let mut block = None;
        let mut allow_planning = !preamble;
        let mut allow_properties = !preamble;
        while let Some(&(_, line)) = self.lines.peek() {
            if block.is_none() && headline(line).is_some() {
                break;
            }
            self.lines.next();
            let trimmed = line.trim();
            let directive = trimmed.strip_prefix("#+").unwrap_or_default();

            if let Some(name) = block {
                if block_name(directive, "END_").is_some_and(|end| end.eq_ignore_ascii_case(name)) {
                    block = None;
                } else if collect_body && !name.eq_ignore_ascii_case("COMMENT") {
                    section.body.push(line);
                }
                continue;
            }

            let planning = allow_planning
                && ["SCHEDULED:", "DEADLINE:", "CLOSED:"]
                    .iter()
                    .any(|prefix| trimmed.starts_with(prefix));
            allow_planning = false;
            if planning {
                continue;
            }

            if let Some(name) = drawer_name(trimmed)
                && let Some(length) = self.drawer_length()
            {
                let properties = allow_properties && name.eq_ignore_ascii_case("PROPERTIES");
                for (_, entry) in self.lines.by_ref().take(length - 1) {
                    if properties {
                        add_property(&mut section.properties, entry.trim());
                    }
                }
                self.lines.next();
            } else if let Some(name) = block_name(directive, "BEGIN_") {
                block = Some(name);
            } else if let Some((key, value)) = directive.split_once(':') {
                if preamble {
                    section.keywords.push((key, value.trim()));
                }
            } else if collect_body && trimmed != "#" && !trimmed.starts_with("# ") {
                section.body.push(line);
            }
            allow_properties = false;
        }
        section
    }

    fn drawer_length(&self) -> Option<usize> {
        self.lines
            .clone()
            .take_while(|&(_, line)| headline(line).is_none())
            .position(|(_, line)| line.trim().eq_ignore_ascii_case(":END:"))
            .map(|position| position + 1)
    }
}

fn headline(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&ch| ch == b'*').count();
    (level > 0 && line.as_bytes().get(level) == Some(&b' ')).then(|| (level, line[level..].trim()))
}

fn block_name<'a>(directive: &'a str, prefix: &str) -> Option<&'a str> {
    if !directive.get(..prefix.len())?.eq_ignore_ascii_case(prefix) {
        return None;
    }
    directive[prefix.len()..].split_whitespace().next()
}

fn drawer_name(line: &str) -> Option<&str> {
    let name = line.strip_prefix(':')?.strip_suffix(':')?;
    (!name.is_empty()
        && !name.eq_ignore_ascii_case("END")
        && name
            .chars()
            .all(|ch| ch.is_alphanumeric() || "_-".contains(ch)))
    .then_some(name)
}

fn add_property(properties: &mut BTreeMap<String, String>, line: &str) {
    if let Some((key, value)) = line.strip_prefix(':').and_then(|line| line.split_once(':'))
        && !key.is_empty()
        && !key.contains(char::is_whitespace)
        && (value.is_empty() || value.starts_with(char::is_whitespace))
    {
        let value = value.trim();
        if let Some(key) = key.strip_suffix('+').filter(|key| !key.is_empty()) {
            let property = properties.entry(key.to_ascii_uppercase()).or_default();
            if !property.is_empty() && !value.is_empty() {
                property.push(' ');
            }
            property.push_str(value);
        } else {
            properties.insert(key.to_ascii_uppercase(), value.to_owned());
        }
    }
}
