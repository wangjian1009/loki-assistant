use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use log::{LevelFilter, Log, Metadata, Record};
use time::{OffsetDateTime, format_description::well_known::Iso8601};

const MAX_ENTRIES: usize = 500;

struct FileLogger(mpsc::Sender<Option<String>>);

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= LevelFilter::Info
            && (metadata.target() == "loki_services"
                || metadata.target().starts_with("loki_services::"))
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let now = OffsetDateTime::now_utc();
            let date = now.date().format(&Iso8601::DATE).expect("日期可格式化");
            let timestamp = format!(
                "{date} {:02}:{:02}:{:02}.{:03}",
                now.hour(),
                now.minute(),
                now.second(),
                now.millisecond(),
            );
            let target = record
                .target()
                .strip_prefix("loki_services::")
                .unwrap_or("main");
            let thread = thread::current();
            let thread = single_line(thread.name().unwrap_or("worker")).replace(']', "_");
            let message = single_line(&record.args().to_string());
            let _ = self.0.send(Some(format!(
                "{timestamp} [{thread}] {} services.{} - {message}",
                record.level().as_str(),
                target.replace("::", "."),
            )));
        }
    }

    fn flush(&self) {}
}

pub struct LogWorker {
    sender: mpsc::Sender<Option<String>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LogWorker {
    pub fn start(path: PathBuf) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<Option<String>>();
        let worker = thread::Builder::new()
            .name("loki-file-log".into())
            .spawn(move || {
                while let Ok(Some(entry)) = receiver.recv() {
                    if let Err(error) = write_entry(&path, &entry) {
                        eprintln!("写入 services 日志失败：{error}");
                        eprintln!("{entry}");
                    }
                }
            })?;
        let worker = Self {
            sender,
            worker: Some(worker),
        };
        log::set_boxed_logger(Box::new(FileLogger(worker.sender.clone())))
            .map_err(io::Error::other)?;
        log::set_max_level(LevelFilter::Info);
        Ok(worker)
    }
}

impl Drop for LogWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(None);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\r' | '\n' | '\u{2028}' | '\u{2029}' => ' ',
            character if character.is_control() => ' ',
            character => character,
        })
        .collect()
}

fn write_entry(path: &Path, entry: &str) -> io::Result<()> {
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path.parent().expect("日志路径包含目录"))?;
    let previous = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut entries: Vec<_> = previous.lines().rev().take(MAX_ENTRIES - 1).collect();
    entries.reverse();
    let temporary = path.with_extension(format!("log.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        for line in entries {
            writeln!(file, "{line}")?;
        }
        writeln!(file, "{entry}")?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
