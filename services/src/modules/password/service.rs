use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    fmt::Debug,
    fs::{self, DirBuilder, OpenOptions},
    future::Future,
    io::{ErrorKind, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use super::payload;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use zeroize::Zeroizing;

use crate::process::{Process, WeakProcess};

use crate::modules::config::{executable, model::ExecutableMetadata};

pub(crate) const SYSTEM_PASSWORD_PREFIX: &str = "loki-assistant/network/";

#[derive(Debug)]
pub(crate) struct PasswordEnvironment {
    pub(crate) program: String,
    pub(crate) store: PathBuf,
    pub(crate) recipients: Vec<String>,
    pub(crate) signing_keys: Vec<String>,
    options: Vec<String>,
}

impl PasswordEnvironment {
    pub(crate) fn from_env() -> Result<Self, PasswordError> {
        let paths = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .unwrap_or_default();
        let program = executable::find(
            &ExecutableMetadata {
                default_program: "gpg".into(),
                search_paths: vec!["/opt/homebrew/bin".into(), "/usr/local/bin".into()],
            },
            &paths,
        )
        .ok_or(PasswordError::Process)?;
        let store = store_path(
            std::env::var_os("PASSWORD_STORE_DIR"),
            std::env::var_os("HOME"),
        )?;
        let mut environment = Self::new(program, store);
        environment.recipients = env_words("PASSWORD_STORE_KEY");
        environment.signing_keys = env_words("PASSWORD_STORE_SIGNING_KEY");
        environment.options = env_words("PASSWORD_STORE_GPG_OPTS");
        Ok(environment)
    }

    pub(crate) fn new(program: String, store: PathBuf) -> Self {
        Self {
            program,
            store,
            recipients: Vec::new(),
            signing_keys: Vec::new(),
            options: Vec::new(),
        }
    }
}

pub(super) fn store_path(
    store: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, PasswordError> {
    let store = store
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".password-store"))
        })
        .ok_or(PasswordError::NotConfigured)?;
    std::path::absolute(store).map_err(|_| PasswordError::StoreUnavailable)
}

fn env_words(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

pub(crate) type CommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CommandOutput, PasswordError>> + Send + 'a>>;

pub(crate) trait CommandRunner: Debug + Send + Sync {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
        input: Option<&'a [u8]>,
    ) -> CommandFuture<'a>;
}

// 不派生 Debug，避免诊断输出包含解密结果。
pub(crate) struct CommandOutput {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        args: &'a [String],
        input: Option<&'a [u8]>,
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            let mut child = Command::new(program)
                .args(args)
                .env("LC_ALL", "C")
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| PasswordError::Process)?;
            let stdin = child.stdin.take();
            let write = async move {
                if let (Some(mut stdin), Some(input)) = (stdin, input) {
                    stdin.write_all(input).await?;
                }
                Ok::<_, std::io::Error>(())
            };
            let (output, written) = tokio::join!(child.wait_with_output(), write);
            let output = output.map_err(|_| PasswordError::Process)?;
            if output.status.success() {
                written.map_err(|_| PasswordError::Process)?;
            }
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
    #[error("无法从服务启动环境确定密码库。")]
    NotConfigured,
    #[error("本地密码库不可用，请检查目录和访问权限。")]
    StoreUnavailable,
    #[error("密码条目不存在。")]
    EntryNotFound,
    #[error("file 必须是密码库内的相对 .gpg 文件路径，不允许路径穿越、隐藏路径或库外符号链接。")]
    InvalidPath,
    #[error("无法运行 GPG，请检查可执行程序及其依赖。")]
    Process,
    #[error("GPG 执行失败（退出码 {status:?}），请检查 GPG agent 和密钥权限。")]
    CommandFailed { status: Option<i32> },
    #[error("GPG 操作超时，请检查 GPG agent 或 pinentry。")]
    Timeout,
    #[error("密码条目解密后不是有效的 UTF-8 内容。")]
    InvalidOutput,
    #[error("无法确定或验证密码条目的 GPG 收件人。")]
    Recipients,
}

#[derive(Debug)]
pub struct PasswordService {
    store: Arc<Store>,
    agent: Process<SessionKeys>,
    writers: Process<HashMap<String, WeakProcess<()>>>,
}

#[derive(Debug)]
struct Store {
    environment: Option<PasswordEnvironment>,
    runner: Arc<dyn CommandRunner>,
    timeout: Duration,
    #[cfg(test)]
    fixture_payload: bool,
}

struct SessionKey {
    algorithm: u8,
    bytes: Zeroizing<Vec<u8>>,
}

#[derive(Default)]
struct SessionKeys {
    keys: VecDeque<([u8; 32], Instant, Arc<SessionKey>)>,
}

impl SessionKeys {
    async fn key(
        &mut self,
        store: &Store,
        snapshot: &CiphertextSnapshot,
    ) -> Result<Arc<SessionKey>, PasswordError> {
        self.keys.retain(|(_, expiry, _)| *expiry > Instant::now());
        if let Some(index) = self
            .keys
            .iter()
            .position(|(hash, _, _)| *hash == snapshot.digest)
        {
            let entry = self.keys.remove(index).expect("已找到会话密钥");
            let key = entry.2.clone();
            self.keys.push_back(entry);
            return Ok(key);
        }

        let key = Arc::new(store.session_key(snapshot).await?);
        if self.keys.len() == 64 {
            self.keys.pop_front();
        }
        self.keys.push_back((
            snapshot.digest,
            Instant::now() + Duration::from_secs(300),
            key.clone(),
        ));
        Ok(key)
    }
}

// gpg-agent 和 Rust 解密任务读取同一份密文。
struct CiphertextSnapshot {
    directory: PathBuf,
    digest: [u8; 32],
}

impl CiphertextSnapshot {
    fn create(source: &Path) -> Result<Self, PasswordError> {
        let directory = std::env::temp_dir().join(format!("loki-gpg-{}", uuid::Uuid::new_v4()));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|_| PasswordError::StoreUnavailable)?;
        let mut snapshot = Self {
            directory,
            digest: [0; 32],
        };
        let mut ciphertext = fs::File::open(source).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                PasswordError::EntryNotFound
            } else {
                PasswordError::StoreUnavailable
            }
        })?;
        let mut copy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(snapshot.file())
            .map_err(|_| PasswordError::StoreUnavailable)?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 16384];
        loop {
            let size = ciphertext
                .read(&mut buffer)
                .map_err(|_| PasswordError::StoreUnavailable)?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
            copy.write_all(&buffer[..size])
                .map_err(|_| PasswordError::StoreUnavailable)?;
        }
        snapshot.digest = digest.finalize().into();
        Ok(snapshot)
    }

    fn file(&self) -> PathBuf {
        self.directory.join("entry.gpg")
    }
}

impl Drop for CiphertextSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl PasswordService {
    pub(crate) fn new(environment: Option<PasswordEnvironment>) -> Self {
        Self::with_runner(
            environment,
            Arc::new(ProcessCommandRunner),
            Duration::from_secs(30),
        )
    }

    pub(crate) fn with_runner(
        environment: Option<PasswordEnvironment>,
        runner: Arc<dyn CommandRunner>,
        timeout: Duration,
    ) -> Self {
        Self {
            store: Arc::new(Store {
                environment,
                runner,
                timeout,
                #[cfg(test)]
                fixture_payload: false,
            }),
            agent: Process::start(SessionKeys::default()),
            writers: Process::start(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_fixture_runner(
        environment: Option<PasswordEnvironment>,
        runner: Arc<dyn CommandRunner>,
        timeout: Duration,
    ) -> Self {
        let mut service = Self::with_runner(environment, runner, timeout);
        Arc::get_mut(&mut service.store)
            .expect("独立的密码库")
            .fixture_payload = true;
        service
    }

    async fn writer(&self, file: &str) -> Result<Process<()>, PasswordError> {
        validate_file(file)?;
        // 普通文件路径以写入规则校验，拒绝目录别名和符号链接。
        let file = file.to_owned();
        Ok(self
            .writers
            .call(move |writers| {
                writers.retain(|_, writer| writer.upgrade().is_some());
                if let Some(writer) = writers.get(&file).and_then(WeakProcess::upgrade) {
                    return writer;
                }
                let writer = Process::start(());
                writers.insert(file, writer.downgrade());
                writer
            })
            .await)
    }

    pub(crate) async fn save(&self, file: &str, secret: &str) -> Result<(), PasswordError> {
        let writer = self.writer(file).await?;
        let store = self.store.clone();
        let (file, secret) = (file.to_owned(), secret.to_owned());
        let keep_alive = writer.clone();
        writer
            .run(async move {
                let _writer = keep_alive;
                store.save(&file, &secret).await
            })
            .await
    }

    pub(crate) async fn save_multiline(
        &self,
        file: &str,
        contents: &str,
    ) -> Result<(), PasswordError> {
        let writer = self.writer(file).await?;
        let store = self.store.clone();
        let (file, contents) = (file.to_owned(), contents.to_owned());
        let keep_alive = writer.clone();
        writer
            .run(async move {
                let _writer = keep_alive;
                store.save_multiline(&file, &contents).await
            })
            .await
    }

    pub(crate) async fn remove(&self, file: &str) -> Result<(), PasswordError> {
        let writer = self.writer(file).await?;
        let store = self.store.clone();
        let file = file.to_owned();
        let keep_alive = writer.clone();
        writer
            .run(async move {
                let _writer = keep_alive;
                store.remove(&file).await
            })
            .await
    }

    pub async fn entries(&self, query: &str) -> Result<Vec<String>, PasswordError> {
        self.store.entries(query).await
    }

    pub async fn fetch(&self, file: &str) -> Result<String, PasswordError> {
        self.fetch_entry(file, false).await
    }

    pub(super) async fn fetch_public(&self, file: &str) -> Result<String, PasswordError> {
        self.fetch_entry(file, true).await
    }

    async fn fetch_entry(&self, file: &str, public: bool) -> Result<String, PasswordError> {
        let snapshot = Arc::new(self.store.snapshot(file, public).await?);
        let store = self.store.clone();
        let agent_snapshot = snapshot.clone();
        let key = self
            .agent
            .with(move |keys| Box::pin(async move { keys.key(&store, &agent_snapshot).await }))
            .await?;
        #[cfg(test)]
        if self.store.fixture_payload {
            return self.store.decrypt_fixture(&snapshot).await;
        }
        tokio::task::spawn_blocking(move || {
            payload::decrypt(&snapshot.file(), key.algorithm, key.bytes.as_slice())
                .map_err(|_| PasswordError::InvalidOutput)
        })
        .await
        .map_err(|_| PasswordError::InvalidOutput)?
    }

    pub(crate) async fn missing_entries(
        &self,
        paths: HashSet<String>,
    ) -> Result<HashSet<String>, PasswordError> {
        self.store.missing_entries(paths).await
    }
}

impl Store {
    async fn save(&self, file: &str, secret: &str) -> Result<(), PasswordError> {
        if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control) {
            return Err(PasswordError::InvalidRequest);
        }
        self.write_entry(file, secret).await
    }

    async fn save_multiline(&self, file: &str, contents: &str) -> Result<(), PasswordError> {
        if contents.is_empty()
            || contents.len() > 16 * 1024
            || contents.chars().any(|ch| ch.is_control() && ch != '\n')
        {
            return Err(PasswordError::InvalidRequest);
        }
        self.write_entry(file, contents).await
    }

    async fn write_entry(&self, file: &str, contents: &str) -> Result<(), PasswordError> {
        validate_file(file)?;
        let store = self.store().await?;
        let file = file.to_owned();
        let lookup_store = store.clone();
        let lookup_file = file.clone();
        let recipients_file = tokio::task::spawn_blocking(move || {
            check_write_path(&lookup_store, &lookup_file)?;
            recipient_file(&lookup_store, &lookup_file)
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)??;
        let recipients = self.recipients(&store, recipients_file).await?;
        let mut args = vec![
            "--output".into(),
            "-".into(),
            "--compress-algo".into(),
            "none".into(),
            "--no-encrypt-to".into(),
            "--encrypt".into(),
        ];
        for recipient in recipients {
            args.extend(["--recipient".into(), recipient]);
        }
        let input = format!("{contents}\n");
        let ciphertext = self.run(args, Some(input.as_bytes())).await?;
        if ciphertext.is_empty() {
            return Err(PasswordError::InvalidOutput);
        }
        tokio::task::spawn_blocking(move || write_ciphertext(&store, &file, &ciphertext))
            .await
            .map_err(|_| PasswordError::StoreUnavailable)?
    }

    async fn recipients(
        &self,
        store: &Path,
        recipient_file: Option<PathBuf>,
    ) -> Result<Vec<String>, PasswordError> {
        let environment = self.environment()?;
        if !environment.recipients.is_empty() {
            return Ok(environment.recipients.clone());
        }
        let path = recipient_file.ok_or(PasswordError::Recipients)?;
        let path = fs::canonicalize(path).map_err(|_| PasswordError::Recipients)?;
        if !path.starts_with(store) || !path.is_file() {
            return Err(PasswordError::Recipients);
        }
        if !environment.signing_keys.is_empty() {
            let mut signature = path.as_os_str().to_owned();
            signature.push(".sig");
            let signature = fs::canonicalize(signature).map_err(|_| PasswordError::Recipients)?;
            if !signature.starts_with(store) || !signature.is_file() {
                return Err(PasswordError::Recipients);
            }
            let output = self
                .run(
                    vec![
                        "--status-fd".into(),
                        "1".into(),
                        "--verify".into(),
                        "--".into(),
                        gpg_path(&signature)?,
                        gpg_path(&path)?,
                    ],
                    None,
                )
                .await?;
            let status = String::from_utf8(output).map_err(|_| PasswordError::Recipients)?;
            if !status
                .lines()
                .filter_map(|line| line.strip_prefix("[GNUPG:] VALIDSIG "))
                .any(|signature| {
                    let mut fields = signature.split_whitespace();
                    let signer = fields.next();
                    let primary = fields.last();
                    environment
                        .signing_keys
                        .iter()
                        .any(|key| signer == Some(key.as_str()) || primary == Some(key.as_str()))
                })
            {
                return Err(PasswordError::Recipients);
            }
        }
        let contents = tokio::fs::read_to_string(path)
            .await
            .map_err(|_| PasswordError::Recipients)?;
        let recipients: Vec<_> = contents
            .lines()
            .map(|line| line.split('#').next().unwrap_or_default().trim())
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if recipients.is_empty() {
            return Err(PasswordError::Recipients);
        }
        Ok(recipients)
    }

    async fn remove(&self, file: &str) -> Result<(), PasswordError> {
        validate_file(file)?;
        let store = self.store().await?;
        let file = file.to_owned();
        tokio::task::spawn_blocking(move || {
            check_write_path(&store, &file)?;
            match fs::remove_file(store.join(file)) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err(PasswordError::StoreUnavailable),
            }
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)?
    }

    fn environment(&self) -> Result<&PasswordEnvironment, PasswordError> {
        self.environment
            .as_ref()
            .ok_or(PasswordError::NotConfigured)
    }

    async fn store(&self) -> Result<PathBuf, PasswordError> {
        let store = self.environment()?.store.clone();
        tokio::task::spawn_blocking(move || {
            let store = fs::canonicalize(store).map_err(|_| PasswordError::StoreUnavailable)?;
            if !store.is_dir() {
                return Err(PasswordError::StoreUnavailable);
            }
            Ok(store)
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)?
    }

    async fn run(
        &self,
        operation: Vec<String>,
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, PasswordError> {
        let environment = self.environment()?;
        let mut args = environment.options.clone();
        args.extend(["--batch".into(), "--no-tty".into(), "--quiet".into()]);
        args.extend(operation);
        self.run_gpg(&environment.program, &args, input).await
    }

    async fn run_gpg(
        &self,
        program: &str,
        args: &[String],
        input: Option<&[u8]>,
    ) -> Result<Vec<u8>, PasswordError> {
        let output = timeout(self.timeout, self.runner.run(program, args, input))
            .await
            .map_err(|_| PasswordError::Timeout)??;
        if output.status != Some(0) {
            return Err(PasswordError::CommandFailed {
                status: output.status,
            });
        }
        Ok(output.stdout)
    }

    async fn entries(&self, query: &str) -> Result<Vec<String>, PasswordError> {
        if query.chars().any(char::is_control) {
            return Err(PasswordError::InvalidRequest);
        }
        let store = self.store().await?;
        let query = query.to_lowercase();
        tokio::task::spawn_blocking(move || list_files(&store, &query))
            .await
            .map_err(|_| PasswordError::StoreUnavailable)?
    }

    async fn snapshot(
        &self,
        file: &str,
        public: bool,
    ) -> Result<CiphertextSnapshot, PasswordError> {
        validate_file(file)?;
        if public && file.starts_with(SYSTEM_PASSWORD_PREFIX) {
            return Err(PasswordError::EntryNotFound);
        }
        let store = self.store().await?;
        let file = file.to_owned();
        tokio::task::spawn_blocking(move || {
            let path = checked_file(&store, &file)?;
            if public && internal_path(&store, &path) {
                return Err(PasswordError::EntryNotFound);
            }
            CiphertextSnapshot::create(&path)
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)?
    }

    async fn session_key(
        &self,
        snapshot: &CiphertextSnapshot,
    ) -> Result<SessionKey, PasswordError> {
        let status = Zeroizing::new(
            self.run(
                vec![
                    "--status-fd".into(),
                    "1".into(),
                    "--show-only-session-key".into(),
                    "--decrypt".into(),
                    "--".into(),
                    gpg_path(&snapshot.file())?,
                ],
                None,
            )
            .await?,
        );
        let key = status
            .split(|byte| *byte == b'\n')
            .find_map(|line| line.strip_prefix(b"[GNUPG:] SESSION_KEY "))
            .ok_or(PasswordError::InvalidOutput)?;
        let Some(colon) = key.iter().position(|byte| *byte == b':') else {
            return Err(PasswordError::InvalidOutput);
        };
        if colon == 0
            || key.len() <= colon + 1
            || !key[colon + 1..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(PasswordError::InvalidOutput);
        }
        // GnuPG 在 AEAD 状态中以「对称算法.AEAD 算法」表示算法编号。
        let algorithms =
            std::str::from_utf8(&key[..colon]).map_err(|_| PasswordError::InvalidOutput)?;
        let algorithm = if let Some((cipher, aead)) = algorithms.split_once('.') {
            aead.parse::<u8>()
                .map_err(|_| PasswordError::InvalidOutput)?;
            cipher
        } else {
            algorithms
        };
        let algorithm = algorithm
            .parse::<u8>()
            .map_err(|_| PasswordError::InvalidOutput)?;
        let mut bytes = Zeroizing::new(Vec::new());
        let mut characters = key[colon + 1..].chunks_exact(2);
        if !characters.remainder().is_empty() {
            return Err(PasswordError::InvalidOutput);
        }
        for pair in &mut characters {
            let hex = std::str::from_utf8(pair).map_err(|_| PasswordError::InvalidOutput)?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|_| PasswordError::InvalidOutput)?);
        }
        Ok(SessionKey { algorithm, bytes })
    }

    #[cfg(test)]
    async fn decrypt_fixture(
        &self,
        snapshot: &CiphertextSnapshot,
    ) -> Result<String, PasswordError> {
        let output = self
            .run(
                vec![
                    "--output".into(),
                    "-".into(),
                    "--decrypt".into(),
                    "--".into(),
                    gpg_path(&snapshot.file())?,
                ],
                None,
            )
            .await?;
        String::from_utf8(output).map_err(|_| PasswordError::InvalidOutput)
    }

    async fn missing_entries(
        &self,
        paths: HashSet<String>,
    ) -> Result<HashSet<String>, PasswordError> {
        let store = self.store().await?;
        tokio::task::spawn_blocking(move || {
            paths
                .into_iter()
                .filter(|path| {
                    matches!(
                        checked_file(&store, path),
                        Err(PasswordError::EntryNotFound)
                    )
                })
                .collect()
        })
        .await
        .map_err(|_| PasswordError::StoreUnavailable)
    }
}

fn gpg_path(path: &Path) -> Result<String, PasswordError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(PasswordError::InvalidPath)
}

fn recipient_file(store: &Path, file: &str) -> Result<Option<PathBuf>, PasswordError> {
    let entry = store.join(file);
    let parent = entry.parent().ok_or(PasswordError::InvalidPath)?;
    for path in parent
        .ancestors()
        .take_while(|directory| directory.starts_with(store))
        .map(|directory| directory.join(".gpg-id"))
    {
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(PasswordError::Recipients),
        }
    }
    Ok(None)
}

fn write_ciphertext(store: &Path, file: &str, ciphertext: &[u8]) -> Result<(), PasswordError> {
    check_write_path(store, file)?;
    let destination = store.join(file);
    let parent = destination.parent().ok_or(PasswordError::InvalidPath)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| PasswordError::StoreUnavailable)?;
    let temporary = parent.join(format!(".password-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        output.write_all(ciphertext)?;
        output.sync_all()?;
        fs::rename(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|_| PasswordError::StoreUnavailable)
}

fn check_write_path(store: &Path, file: &str) -> Result<(), PasswordError> {
    let mut parts = file.split('/').peekable();
    let mut path = store.to_owned();
    while let Some(part) = parts.next() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let valid = if parts.peek().is_none() {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                };
                if !valid {
                    return Err(PasswordError::InvalidPath);
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => break,
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

fn checked_file(store: &Path, file: &str) -> Result<PathBuf, PasswordError> {
    validate_file(file)?;
    let path = fs::canonicalize(store.join(file)).map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
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
    Ok(path)
}

fn internal_path(store: &Path, path: &Path) -> bool {
    path.starts_with(store.join(SYSTEM_PASSWORD_PREFIX))
}

fn list_files(store: &Path, query: &str) -> Result<Vec<String>, PasswordError> {
    let mut files = Vec::new();
    let mut directories = vec![store.to_owned()];
    while let Some(directory) = directories.pop() {
        let entries = fs::read_dir(directory).map_err(|_| PasswordError::StoreUnavailable)?;
        for entry in entries {
            let entry = entry.map_err(|_| PasswordError::StoreUnavailable)?;
            let path = entry.path();
            let Some(relative) = path.strip_prefix(store).ok().and_then(Path::to_str) else {
                continue;
            };
            if relative.split('/').any(|part| part.starts_with('.')) || internal_path(store, &path)
            {
                continue;
            }
            if entry
                .file_type()
                .map_err(|_| PasswordError::StoreUnavailable)?
                .is_dir()
            {
                directories.push(path);
            } else if relative.to_lowercase().contains(query) {
                match checked_file(store, relative) {
                    Ok(target) if !internal_path(store, &target) => files.push(relative.to_owned()),
                    Ok(_) | Err(PasswordError::InvalidPath | PasswordError::EntryNotFound) => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    files.sort();
    Ok(files)
}
