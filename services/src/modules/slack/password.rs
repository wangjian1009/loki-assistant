use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

use crate::modules::password::{
    config::PasswordConfig,
    service::{CommandFuture, CommandOutput, PasswordError, PasswordService},
};

type PassWriter = for<'a> fn(&'a str, &'a Path, &'a [String], &'a str) -> CommandFuture<'a>;

pub(super) struct SlackPasswords {
    config: PasswordConfig,
    reader: Arc<PasswordService>,
    write: PassWriter,
    timeout: Duration,
}

impl SlackPasswords {
    pub fn new(config: PasswordConfig, reader: Arc<PasswordService>) -> Self {
        Self::with_writer(config, reader, |program, store, args, input| {
            Box::pin(Self::execute(program, store, args, input))
        })
    }

    pub fn with_writer(
        config: PasswordConfig,
        reader: Arc<PasswordService>,
        write: PassWriter,
    ) -> Self {
        Self {
            config,
            reader,
            write,
            timeout: Duration::from_secs(30),
        }
    }

    pub async fn fetch(&self, file: &str) -> Result<String, PasswordError> {
        self.reader.fetch(file).await
    }

    async fn execute(
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

    pub(super) async fn save(&self, file: &str, secret: &str) -> Result<(), PasswordError> {
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
        let output = timeout(self.timeout, (self.write)(&program, &store, &args, &input))
            .await
            .map_err(|_| PasswordError::Timeout)??;
        if output.status != Some(0) {
            return Err(PasswordError::CommandFailed {
                status: output.status,
            });
        }
        Ok(())
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

pub(super) fn validate_file(file: &str) -> Result<(), PasswordError> {
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
