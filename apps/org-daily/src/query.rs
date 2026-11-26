use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, ensure};

#[derive(Clone)]
pub struct PropertyFilter {
    name: String,
    value: Option<String>,
}

impl PropertyFilter {
    pub fn parse(input: &str) -> Result<Self> {
        let (name, value) = input
            .split_once('=')
            .map_or((input, None), |(name, value)| {
                (name, Some(value.to_owned()))
            });
        Self::new(name, value)
    }

    fn new(name: &str, value: Option<String>) -> Result<Self> {
        ensure!(
            !name.is_empty() && !name.contains([':', '+']) && !name.contains(char::is_whitespace),
            "属性应为 KEY 或 KEY=VALUE"
        );
        Ok(Self {
            name: name.to_ascii_uppercase(),
            value,
        })
    }

    pub fn matches(&self, properties: &BTreeMap<String, String>) -> bool {
        properties
            .get(&self.name)
            .is_some_and(|value| self.value.as_ref().is_none_or(|expected| expected == value))
    }
}

#[derive(Clone)]
pub enum Expression {
    Tag(String),
    Property(PropertyFilter),
    Not(Box<Self>),
    And(Vec<Self>),
    Or(Vec<Self>),
}

impl Expression {
    pub fn parse(input: &str) -> Result<Self> {
        let mut parser = Parser {
            input,
            rest: input.trim_start(),
        };
        let expression = parser.parse_or(0)?;
        ensure!(
            parser.rest.is_empty(),
            parser.error("此处应为 AND、OR 或表达式结尾")
        );
        Ok(expression)
    }

    pub fn matches(&self, tags: &BTreeSet<&str>, properties: &BTreeMap<String, String>) -> bool {
        match self {
            Self::Tag(tag) => tags.contains(tag.as_str()),
            Self::Property(filter) => filter.matches(properties),
            Self::Not(expression) => !expression.matches(tags, properties),
            Self::And(terms) => terms.iter().all(|term| term.matches(tags, properties)),
            Self::Or(terms) => terms.iter().any(|term| term.matches(tags, properties)),
        }
    }
}

struct Parser<'a> {
    input: &'a str,
    rest: &'a str,
}

impl<'a> Parser<'a> {
    fn parse_or(&mut self, depth: usize) -> Result<Expression> {
        let first = self.parse_and(depth)?;
        if !self.eat_keyword("OR") {
            return Ok(first);
        }
        let mut terms = vec![first, self.parse_and(depth)?];
        while self.eat_keyword("OR") {
            terms.push(self.parse_and(depth)?);
        }
        Ok(Expression::Or(terms))
    }

    fn parse_and(&mut self, depth: usize) -> Result<Expression> {
        let first = self.parse_term(depth)?;
        if !self.eat_keyword("AND") {
            return Ok(first);
        }
        let mut terms = vec![first, self.parse_term(depth)?];
        while self.eat_keyword("AND") {
            terms.push(self.parse_term(depth)?);
        }
        Ok(Expression::And(terms))
    }

    fn parse_term(&mut self, depth: usize) -> Result<Expression> {
        ensure!(depth <= 64, self.error("括号和 NOT 的嵌套超过 64 层"));
        if self.eat_keyword("NOT") {
            return Ok(Expression::Not(Box::new(self.parse_term(depth + 1)?)));
        }
        if self.eat_char('(') {
            let expression = self.parse_or(depth + 1)?;
            ensure!(self.eat_char(')'), self.error("缺少右括号"));
            return Ok(expression);
        }
        if self.eat_keyword("tag") {
            ensure!(self.eat_char(':'), self.error("tag 后需要冒号"));
            let tag = self.value()?;
            ensure!(!tag.is_empty(), self.error("标签不能为空"));
            return Ok(Expression::Tag(tag));
        }
        if self.eat_keyword("prop") {
            ensure!(self.eat_char(':'), self.error("prop 后需要冒号"));
            let name = self.word();
            let mut filter =
                PropertyFilter::new(name, None).map_err(|_| self.error("属性名应为有效的 KEY"))?;
            self.advance(name.len());
            if self.eat_char('=') {
                filter.value = Some(self.value()?);
            }
            return Ok(Expression::Property(filter));
        }
        Err(self.error("此处应为 tag:标签、prop:属性、NOT 或左括号"))
    }

    fn value(&mut self) -> Result<String> {
        if self.rest.starts_with('"') {
            let mut strings = serde_json::Deserializer::from_str(self.rest).into_iter::<String>();
            let value = strings
                .next()
                .expect("输入以双引号开头")
                .map_err(|err| self.error(&format!("双引号字符串无效（{err}）")))?;
            self.advance(strings.byte_offset());
            return Ok(value);
        }
        let value = self.word();
        ensure!(!value.is_empty(), self.error("缺少标签或属性值"));
        self.advance(value.len());
        Ok(value.to_owned())
    }

    fn eat_char(&mut self, ch: char) -> bool {
        if self.rest.starts_with(ch) {
            self.advance(ch.len_utf8());
            return true;
        }
        false
    }

    fn eat_keyword(&mut self, keyword: &str) -> bool {
        let word = self.word();
        if word.eq_ignore_ascii_case(keyword) {
            self.advance(word.len());
            return true;
        }
        false
    }

    fn word(&self) -> &'a str {
        let end = self
            .rest
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '(' | ')' | ':' | '=' | '"'))
            .unwrap_or(self.rest.len());
        &self.rest[..end]
    }

    fn advance(&mut self, bytes: usize) {
        self.rest = self.rest[bytes..].trim_start();
    }

    fn error(&self, message: &str) -> anyhow::Error {
        let position = self.input[..self.input.len() - self.rest.len()]
            .chars()
            .count()
            + 1;
        anyhow!("第 {position} 个字符处，{message}")
    }
}
