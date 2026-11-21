use std::{
    collections::HashSet,
    fmt::Debug,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

use super::config::PasswordConfig;

type PassWriter = for<'a> fn(&'a str, &'a Path, &'a [String], &'a str) -> CommandFuture<'a>;

pub type CommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CommandOutput, PasswordError>> + Send + 'a>>;

/// 只允许模块业务入口构造 ls 和 show 参数；测试替身不访问真实密码库。
pub trait CommandRunner: Debug + Send + Sync {
    fn run<'a>(
        &'a self,
        program: &'a str,
        store: &'a Path,
        args: &'a [String],
    ) -> CommandFuture<'a>;
}

// 不派生 Debug，避免诊断输出包含明文或 GPG 的原始标准错误。
pub struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
}

#[derive(Debug)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        store: &'a Path,
        args: &'a [String],
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            let mut command = Command::new(program);
            // pass 会继续调用 gpg、tree 等程序；补入已解析 pass 所在目录，兼容 Homebrew 服务的 PATH。
            let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
                .map(|value| std::env::split_paths(&value).collect())
                .unwrap_or_default();
            if let Some(parent) = Path::new(program).parent() {
                paths.push(parent.to_owned());
            }
            command
                .args(args)
                .env(
                    "PATH",
                    std::env::join_paths(paths).map_err(|_| PasswordError::Process)?,
                )
                .env("PASSWORD_STORE_DIR", store)
                .env("PASSWORD_STORE_ENABLE_EXTENSIONS", "false")
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .kill_on_drop(true);
            let output = command.output().await.map_err(|_| PasswordError::Process)?;
            // pass/GPG 标准错误可能含敏感内容，只用退出码生成稳定的中文错误。
            Ok(CommandOutput {
                status: output.status.code(),
                stdout: output.stdout,
            })
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("请求格式无效，请按接口约定提供查询参数。")]
    InvalidRequest,
    #[error("请先配置密码管理模块的本地 pass 目录和可执行程序。")]
    NotConfigured,
    #[error("本地 pass 目录不可用，请检查目录配置和访问权限。")]
    StoreUnavailable,
    #[error("密码条目不存在。")]
    EntryNotFound,
    #[error("file 必须是密码库内的相对 .gpg 文件路径，不允许路径穿越、隐藏路径或库外符号链接。")]
    InvalidPath,
    #[error("无法运行 pass，请检查可执行程序及其依赖。")]
    Process,
    #[error("pass 执行失败（退出码 {status:?}），请检查 pass、GPG agent 和解密权限。")]
    CommandFailed { status: Option<i32> },
    #[error("pass 查询超时，请检查 GPG agent 或 pinentry。")]
    Timeout,
    #[error("pass 返回了无法识别的列表或非 UTF-8 内容。")]
    InvalidOutput,
}

#[derive(Debug)]
pub struct PasswordService {
    config: PasswordConfig,
    runner: Arc<dyn CommandRunner>,
    writer: PassWriter,
    timeout: Duration,
}

impl PasswordService {
    pub fn new(config: PasswordConfig) -> Self {
        Self::with_runner(
            config,
            Arc::new(ProcessCommandRunner),
            Duration::from_secs(30),
        )
    }

    pub fn with_runner(
        config: PasswordConfig,
        runner: Arc<dyn CommandRunner>,
        timeout: Duration,
    ) -> Self {
        Self {
            config,
            runner,
            writer: |program, store, args, input| {
                Box::pin(Self::write_pass(program, store, args, input))
            },
            timeout,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_writer(mut self, writer: PassWriter) -> Self {
        self.writer = writer;
        self
    }

    async fn write_pass(
        program: &str,
        store: &Path,
        args: &[String],
        input: &str,
    ) -> Result<CommandOutput, PasswordError> {
        let mut command = Command::new(program);
        // pass 会继续调用 gpg、tree 等程序；补入已解析 pass 所在目录，兼容 Homebrew 服务的 PATH。
        let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        if let Some(parent) = Path::new(program).parent() {
            paths.push(parent.to_owned());
        }
        command
            .args(args)
            .env(
                "PATH",
                std::env::join_paths(paths).map_err(|_| PasswordError::Process)?,
            )
            .env("PASSWORD_STORE_DIR", store)
            .env("PASSWORD_STORE_ENABLE_EXTENSIONS", "false")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| PasswordError::Process)?;
        {
            let mut stdin = child.stdin.take().ok_or(PasswordError::Process)?;
            stdin
                .write_all(input.as_bytes())
                .await
                .map_err(|_| PasswordError::Process)?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|_| PasswordError::Process)?;
        // pass/GPG 标准错误可能含敏感内容，只用退出码生成稳定的中文错误。
        Ok(CommandOutput {
            status: output.status.code(),
            stdout: output.stdout,
        })
    }
    pub(crate) async fn save(&self, file: &str, secret: &str) -> Result<(), PasswordError> {
        validate_file(file)?;
        if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control) {
            return Err(PasswordError::InvalidRequest);
        }
        let (program, store) = self.settings().await?;
        let (checked_store, checked_file) = (store.clone(), file.to_owned());
        tokio::task::spawn_blocking(move || check_write_path(&checked_store, &checked_file))
            .await
            .map_err(|_| PasswordError::StoreUnavailable)??;
        let entry = file
            .strip_suffix(".gpg")
            .ok_or(PasswordError::InvalidPath)?;
        let args = [
            "insert".into(),
            "--force".into(),
            "--multiline".into(),
            "--".into(),
            entry.into(),
        ];
        let input = format!("{secret}\n");
        let output = timeout(self.timeout, (self.writer)(&program, &store, &args, &input))
            .await
            .map_err(|_| PasswordError::Timeout)??;
        if output.status != Some(0) {
            return Err(PasswordError::CommandFailed {
                status: output.status,
            });
        }
        Ok(())
    }

    pub(crate) async fn remove(&self, file: &str) -> Result<(), PasswordError> {
        validate_file(file)?;
        let (program, store) = self.settings().await?;
        let (checked_store, checked_file) = (store.clone(), file.to_owned());
        tokio::task::spawn_blocking(move || check_write_path(&checked_store, &checked_file))
            .await
            .map_err(|_| PasswordError::StoreUnavailable)??;
        let entry = file
            .strip_suffix(".gpg")
            .ok_or(PasswordError::InvalidPath)?;
        self.run(
            &program,
            &store,
            &["rm".into(), "--force".into(), "--".into(), entry.into()],
        )
        .await?;
        Ok(())
    }

    async fn settings(&self) -> Result<(String, PathBuf), PasswordError> {
        let store = self
            .config
            .store_dir()
            .await
            .map_err(|_| PasswordError::NotConfigured)?;
        let store = store
            .value
            .as_str()
            .filter(|value| Path::new(value).is_absolute() && !value.contains('\0'))
            .ok_or(PasswordError::StoreUnavailable)?
            .to_owned();
        let program = self
            .config
            .pass_path()
            .await
            .map_err(|_| PasswordError::NotConfigured)?;
        let program = program
            .value
            .as_str()
            .filter(|value| !value.is_empty() && !value.contains('\0'))
            .ok_or(PasswordError::NotConfigured)?
            .to_owned();
        let store = tokio::task::spawn_blocking(move || {
            let store =
                std::fs::canonicalize(store).map_err(|_| PasswordError::StoreUnavailable)?;
            if !store.is_dir() {
                return Err(PasswordError::StoreUnavailable);
            }
            Ok(store)
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)??;
        Ok((program, store))
    }

    async fn run(
        &self,
        program: &str,
        store: &Path,
        args: &[String],
    ) -> Result<String, PasswordError> {
        let output = timeout(self.timeout, self.runner.run(program, store, args))
            .await
            .map_err(|_| PasswordError::Timeout)??;
        if output.status != Some(0) {
            return Err(PasswordError::CommandFailed {
                status: output.status,
            });
        }
        String::from_utf8(output.stdout).map_err(|_| PasswordError::InvalidOutput)
    }

    pub async fn entries(&self, query: &str) -> Result<Vec<String>, PasswordError> {
        if query.chars().any(char::is_control) {
            return Err(PasswordError::InvalidRequest);
        }
        let (program, store) = self.settings().await?;
        let output = self.run(&program, &store, &["ls".to_owned()]).await?;
        let mut files = tokio::task::spawn_blocking(move || list_files(&store, &output))
            .await
            .map_err(|_| PasswordError::InvalidOutput)??;
        let query = query.to_lowercase();
        files.retain(|file| file.to_lowercase().contains(&query));
        Ok(files)
    }

    pub async fn fetch(&self, file: &str) -> Result<String, PasswordError> {
        validate_file(file)?;
        let (program, store) = self.settings().await?;
        let (checked_store, checked_file) = (store.clone(), file.to_owned());
        tokio::task::spawn_blocking(move || check_file(&checked_store, &checked_file))
            .await
            .map_err(|_| PasswordError::StoreUnavailable)??;
        // 固定子命令和 -- 防止条目名被识别成 pass 命令、选项或扩展。
        let entry = file
            .strip_suffix(".gpg")
            .ok_or(PasswordError::InvalidPath)?;
        self.run(
            &program,
            &store,
            &["show".to_owned(), "--".to_owned(), entry.to_owned()],
        )
        .await
    }
}

fn check_write_path(store: &Path, file: &str) -> Result<(), PasswordError> {
    let parts: Vec<_> = file.split('/').collect();
    let mut path = store.to_owned();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let regular = if index + 1 == parts.len() {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                };
                if !regular || metadata.file_type().is_symlink() {
                    return Err(PasswordError::InvalidPath);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(PasswordError::StoreUnavailable),
        }
    }
    Ok(())
}

pub(crate) fn validate_file(file: &str) -> Result<(), PasswordError> {
    if !file.ends_with(".gpg")
        || file.starts_with('/')
        || file.contains('\\')
        || file.chars().any(char::is_control)
        || file
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return Err(PasswordError::InvalidPath);
    }
    Ok(())
}

/// 只检查文件元数据及边界；内容只能由 pass show 读取。
fn check_file(store: &Path, file: &str) -> Result<(), PasswordError> {
    validate_file(file)?;
    let path = std::fs::canonicalize(store.join(file)).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            PasswordError::EntryNotFound
        } else {
            PasswordError::StoreUnavailable
        }
    })?;
    if !path.starts_with(store) {
        return Err(PasswordError::InvalidPath);
    }
    if !path.is_file() {
        return Err(PasswordError::EntryNotFound);
    }
    Ok(())
}

/// pass 1.7 的 ls 使用 tree 展示目录；LC_ALL=C 固定为 ASCII 树枝，去掉 ANSI 颜色后恢复相对路径。
fn list_files(store: &Path, output: &str) -> Result<Vec<String>, PasswordError> {
    let ansi = regex::Regex::new(r"\x1b\[[0-9;]*m").expect("固定的 ANSI 正则有效");
    let output = ansi.replace_all(output, "");
    let mut lines = output.lines();
    if lines.next() != Some("Password Store") {
        return Err(PasswordError::InvalidOutput);
    }
    let mut parents: Vec<String> = Vec::new();
    let mut directories = HashSet::new();
    let mut files = Vec::new();
    for line in lines {
        let mut branch = line;
        let mut depth = 0;
        while let Some(rest) = branch
            .strip_prefix("|   ")
            .or_else(|| branch.strip_prefix("    "))
        {
            branch = rest;
            depth += 1;
        }
        let name = branch
            .strip_prefix("|-- ")
            .or_else(|| branch.strip_prefix("`-- "))
            .ok_or(PasswordError::InvalidOutput)?;
        if depth > parents.len() {
            return Err(PasswordError::InvalidOutput);
        }
        parents.truncate(depth);
        // tree 在符号链接后附加目标；仅把名称用于路径检查，不把目标暴露给客户端。
        let prefix = if parents.is_empty() {
            String::new()
        } else {
            format!("{}/", parents.join("/"))
        };
        let name = if store.join(format!("{prefix}{name}")).exists()
            || store.join(format!("{prefix}{name}.gpg")).exists()
        {
            name
        } else {
            name.split(" -> ")
                .next()
                .ok_or(PasswordError::InvalidOutput)?
        };
        if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
            return Err(PasswordError::InvalidOutput);
        }
        let relative = format!("{prefix}{name}");
        let file = format!("{relative}.gpg");
        match check_file(store, &file) {
            Ok(()) => files.push(file),
            Err(PasswordError::EntryNotFound | PasswordError::InvalidPath) => {}
            Err(error) => return Err(error),
        }
        // pass 同样移除目录名结尾的 .gpg；同名显示的两个目录按 tree 的名称顺序恢复。
        let directory = [name.to_owned(), format!("{name}.gpg")]
            .into_iter()
            .find(|candidate| {
                let relative = format!("{prefix}{candidate}");
                store.join(&relative).is_dir() && directories.insert(relative)
            })
            .unwrap_or_else(|| name.to_owned());
        parents.push(directory);
    }
    files.sort();
    files.dedup();
    Ok(files)
}
