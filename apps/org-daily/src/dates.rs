use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{Datelike, NaiveDate};
use walkdir::{DirEntry, WalkDir};

pub fn parse_date(value: &str) -> Result<NaiveDate> {
    let bytes = value.as_bytes();
    ensure!(
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && digits(&bytes[..4])
            && digits(&bytes[5..7])
            && digits(&bytes[8..])
            && &bytes[..4] != b"0000",
        "日期应为 YYYY-MM-DD"
    );
    NaiveDate::parse_from_str(value, "%Y-%m-%d").context("无效的日历日期")
}

pub fn candidates(
    root: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    include_undated: bool,
) -> Result<Vec<(Option<NaiveDate>, PathBuf)>> {
    ensure!(
        !matches!((from, to), (Some(from), Some(to)) if from > to),
        "--from 必须早于或等于 --to"
    );
    ensure!(
        root.metadata()
            .with_context(|| format!("无法打开日记目录 {}", root.display()))?
            .is_dir(),
        "{} 不是目录",
        root.display()
    );

    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || in_range(entry, from, to))
    {
        let entry = entry.with_context(|| format!("无法遍历日记目录 {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(stem) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_suffix(".org"))
        else {
            continue;
        };
        let date = parse_date(stem).ok();
        if date.map_or(include_undated, |date| {
            from.is_none_or(|from| date >= from) && to.is_none_or(|to| date <= to)
        }) {
            files.push((date, entry.into_path()));
        }
    }
    files.sort_unstable_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    Ok(files)
}

fn in_range(entry: &DirEntry, from: Option<NaiveDate>, to: Option<NaiveDate>) -> bool {
    if entry.file_name().as_encoded_bytes().starts_with(b".") {
        return false;
    }
    if !entry.file_type().is_dir() {
        return true;
    }
    if entry.depth() == 1
        && let Some(year) = number(entry.path(), 4)
    {
        return from.is_none_or(|from| year >= from.year())
            && to.is_none_or(|to| year <= to.year());
    }
    if entry.depth() == 2
        && let Some(year) = entry.path().parent().and_then(|path| number(path, 4))
        && let Some(month) = number(entry.path(), 2).filter(|month| (1..=12).contains(month))
    {
        let month = (year, month as u32);
        return from.is_none_or(|date| month >= (date.year(), date.month()))
            && to.is_none_or(|date| month <= (date.year(), date.month()));
    }
    true
}

fn number(path: &Path, width: usize) -> Option<i32> {
    let name = path.file_name()?.to_str()?;
    (name.len() == width && digits(name.as_bytes())).then(|| name.parse().unwrap())
}

fn digits(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_digit)
}
