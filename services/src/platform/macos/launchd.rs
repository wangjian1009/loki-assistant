use std::{
    collections::BTreeMap,
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner: Send + Sync {
    fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput, LaunchdError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput, LaunchdError> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|source| LaunchdError::Io {
                operation: "执行 launchctl",
                source,
            })?;
        Ok(CommandOutput {
            success: output.status.success(),
            status: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchdError {
    #[error("无法确定用户主目录")]
    HomeDirectoryUnavailable,
    #[error("服务配置无效：{0}")]
    InvalidSpec(String),
    #[error("{operation}失败：{message}")]
    CommandFailed {
        operation: &'static str,
        message: String,
    },
    #[error("{operation}失败：{source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchdServiceSpec {
    pub label: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
    pub run_at_load: bool,
    pub keep_alive: bool,
    pub session_type: Option<String>,
}

impl LaunchdServiceSpec {
    pub fn new(label: impl Into<String>, executable: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            executable: executable.into(),
            arguments: Vec::new(),
            working_directory: None,
            environment: BTreeMap::new(),
            stdout_path: None,
            stderr_path: None,
            run_at_load: true,
            keep_alive: true,
            session_type: None,
        }
    }

    pub fn render_plist(&self) -> Result<String, LaunchdError> {
        self.validate()?;
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n",
        );
        append_string(&mut xml, "Label", &self.label);
        xml.push_str("<key>ProgramArguments</key><array>");
        append_array_string(&mut xml, &self.executable.to_string_lossy());
        for argument in &self.arguments {
            append_array_string(&mut xml, argument);
        }
        xml.push_str("</array>\n");
        append_bool(&mut xml, "RunAtLoad", self.run_at_load);
        append_bool(&mut xml, "KeepAlive", self.keep_alive);
        if let Some(session_type) = &self.session_type {
            append_string(&mut xml, "LimitLoadToSessionType", session_type);
        }
        if let Some(path) = &self.working_directory {
            append_string(&mut xml, "WorkingDirectory", &path.to_string_lossy());
        }
        if let Some(path) = &self.stdout_path {
            append_string(&mut xml, "StandardOutPath", &path.to_string_lossy());
        }
        if let Some(path) = &self.stderr_path {
            append_string(&mut xml, "StandardErrorPath", &path.to_string_lossy());
        }
        if !self.environment.is_empty() {
            xml.push_str("<key>EnvironmentVariables</key><dict>");
            for (key, value) in &self.environment {
                xml.push_str("<key>");
                xml.push_str(&escape_xml(key));
                xml.push_str("</key><string>");
                xml.push_str(&escape_xml(value));
                xml.push_str("</string>");
            }
            xml.push_str("</dict>\n");
        }
        xml.push_str("</dict></plist>\n");
        Ok(xml)
    }

    fn validate(&self) -> Result<(), LaunchdError> {
        if self.label.is_empty() || self.label.contains('/') {
            return Err(LaunchdError::InvalidSpec(
                "label 不能为空且不能包含 /".to_owned(),
            ));
        }
        if self.executable.as_os_str().is_empty() {
            return Err(LaunchdError::InvalidSpec("executable 不能为空".to_owned()));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchdStatus {
    NotInstalled,
    Installed,
    Running,
}

#[derive(Clone, Debug)]
pub struct LaunchdManager<R = ProcessCommandRunner> {
    home_directory: PathBuf,
    uid: u32,
    runner: R,
    launchctl: PathBuf,
}

impl LaunchdManager<ProcessCommandRunner> {
    pub fn new() -> Result<Self, LaunchdError> {
        let home_directory = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or(LaunchdError::HomeDirectoryUnavailable)?;
        // macOS 的 LaunchAgent domain 使用当前用户的 effective uid。
        let uid = unsafe { libc::geteuid() } as u32;
        Ok(Self::for_user(home_directory, uid, ProcessCommandRunner))
    }
}

impl<R: CommandRunner> LaunchdManager<R> {
    pub fn for_user(home_directory: impl Into<PathBuf>, uid: u32, runner: R) -> Self {
        Self {
            home_directory: home_directory.into(),
            uid,
            runner,
            launchctl: PathBuf::from("/bin/launchctl"),
        }
    }

    pub fn plist_path(&self, spec: &LaunchdServiceSpec) -> PathBuf {
        self.home_directory
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", spec.label))
    }

    /// 同步登录启动；重复设置可重试，失败时尽量恢复原有 plist 与加载状态。
    pub fn set_login_startup(
        &self,
        spec: &LaunchdServiceSpec,
        enabled: bool,
    ) -> Result<(), LaunchdError> {
        use std::os::unix::fs::PermissionsExt;

        let path = self.plist_path(spec);
        let original = match fs::read(&path) {
            Ok(data) => Some(data),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(LaunchdError::Io {
                    operation: "读取登录启动配置",
                    source,
                });
            }
        };
        if enabled {
            let metadata = fs::metadata(&spec.executable).map_err(|source| LaunchdError::Io {
                operation: "检查托盘可执行文件",
                source,
            })?;
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                return Err(LaunchdError::InvalidSpec(
                    "托盘程序必须是可执行的普通文件，请先构建并检查路径".to_owned(),
                ));
            }
        }
        let was_loaded = self
            .runner
            .run(&self.launchctl, &["print".to_owned(), self.target(spec)])?
            .success;
        let mut loaded = was_loaded;
        let result = (|| {
            if enabled {
                let plist = spec.render_plist()?;
                fs::create_dir_all(path.parent().expect("LaunchAgent 路径应包含父目录")).map_err(
                    |source| LaunchdError::Io {
                        operation: "创建 LaunchAgent 目录",
                        source,
                    },
                )?;
                for output in [&spec.stdout_path, &spec.stderr_path].into_iter().flatten() {
                    if let Some(parent) = output.parent() {
                        fs::create_dir_all(parent).map_err(|source| LaunchdError::Io {
                            operation: "创建托盘日志目录",
                            source,
                        })?;
                    }
                }
                fs::write(&path, plist).map_err(|source| LaunchdError::Io {
                    operation: "写入登录启动配置",
                    source,
                })?;
                self.run_launchctl("启用登录启动", &["enable".to_owned(), self.target(spec)])?;
                if loaded {
                    self.run_launchctl("启动托盘", &["kickstart".to_owned(), self.target(spec)])?;
                } else {
                    self.run_launchctl(
                        "加载托盘",
                        &[
                            "bootstrap".to_owned(),
                            self.domain(),
                            path.to_string_lossy().into_owned(),
                        ],
                    )?;
                    loaded = true;
                }
            } else {
                if loaded {
                    self.run_launchctl("卸载托盘", &["bootout".to_owned(), self.target(spec)])?;
                    loaded = false;
                }
                if path.exists() {
                    fs::remove_file(&path).map_err(|source| LaunchdError::Io {
                        operation: "删除登录启动配置",
                        source,
                    })?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let restored = (|| {
                if let Some(original) = original {
                    fs::write(&path, original).map_err(|source| LaunchdError::Io {
                        operation: "恢复登录启动配置",
                        source,
                    })?;
                } else if path.exists() {
                    fs::remove_file(&path).map_err(|source| LaunchdError::Io {
                        operation: "清理登录启动配置",
                        source,
                    })?;
                }
                if loaded != was_loaded {
                    if was_loaded {
                        self.run_launchctl(
                            "恢复托盘加载",
                            &[
                                "bootstrap".to_owned(),
                                self.domain(),
                                path.to_string_lossy().into_owned(),
                            ],
                        )?;
                    } else {
                        self.run_launchctl(
                            "撤销托盘加载",
                            &["bootout".to_owned(), self.target(spec)],
                        )?;
                    }
                }
                Ok::<_, LaunchdError>(())
            })();
            return match restored {
                Ok(()) => Err(error),
                Err(restore) => Err(LaunchdError::CommandFailed {
                    operation: "同步登录启动",
                    message: format!("{error}；恢复失败：{restore}"),
                }),
            };
        }
        Ok(())
    }

    pub fn install(&self, spec: &LaunchdServiceSpec) -> Result<PathBuf, LaunchdError> {
        let plist = spec.render_plist()?;
        let path = self.plist_path(spec);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| LaunchdError::Io {
                operation: "创建 LaunchAgent 目录",
                source,
            })?;
        }
        for output_path in [&spec.stdout_path, &spec.stderr_path].into_iter().flatten() {
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(|source| LaunchdError::Io {
                    operation: "创建服务日志目录",
                    source,
                })?;
            }
        }
        fs::write(&path, plist).map_err(|source| LaunchdError::Io {
            operation: "写入 LaunchAgent 配置",
            source,
        })?;
        self.run_launchctl(
            "加载 LaunchAgent",
            &[
                "bootstrap".to_owned(),
                self.domain(),
                path.to_string_lossy().into_owned(),
            ],
        )?;
        Ok(path)
    }

    pub fn uninstall(&self, spec: &LaunchdServiceSpec) -> Result<(), LaunchdError> {
        let path = self.plist_path(spec);
        self.run_launchctl(
            "卸载 LaunchAgent",
            &["bootout".to_owned(), self.target(spec)],
        )?;
        if path.exists() {
            fs::remove_file(path).map_err(|source| LaunchdError::Io {
                operation: "删除 LaunchAgent 配置",
                source,
            })?;
        }
        Ok(())
    }

    pub fn start(&self, spec: &LaunchdServiceSpec) -> Result<(), LaunchdError> {
        let path = self.plist_path(spec);
        if !path.exists() {
            return Err(LaunchdError::InvalidSpec(format!(
                "LaunchAgent 尚未注册：{}",
                spec.label
            )));
        }
        let target = self.target(spec);
        let output = self
            .runner
            .run(&self.launchctl, &["print".to_owned(), target.clone()])?;
        if output.success {
            self.run_launchctl("启动 LaunchAgent", &["kickstart".to_owned(), target])
        } else {
            self.run_launchctl(
                "加载并启动 LaunchAgent",
                &[
                    "bootstrap".to_owned(),
                    self.domain(),
                    path.to_string_lossy().into_owned(),
                ],
            )
        }
    }

    pub fn stop(&self, spec: &LaunchdServiceSpec) -> Result<(), LaunchdError> {
        self.run_launchctl(
            "停止 LaunchAgent",
            &["bootout".to_owned(), self.target(spec)],
        )
    }

    pub fn restart(&self, spec: &LaunchdServiceSpec) -> Result<(), LaunchdError> {
        let path = self.plist_path(spec);
        if !path.exists() {
            return Err(LaunchdError::InvalidSpec(format!(
                "LaunchAgent 尚未注册：{}",
                spec.label
            )));
        }
        let target = self.target(spec);
        let output = self
            .runner
            .run(&self.launchctl, &["print".to_owned(), target.clone()])?;
        if output.success {
            self.run_launchctl(
                "重启 LaunchAgent",
                &["kickstart".to_owned(), "-k".to_owned(), target],
            )
        } else {
            self.run_launchctl(
                "加载并启动 LaunchAgent",
                &[
                    "bootstrap".to_owned(),
                    self.domain(),
                    path.to_string_lossy().into_owned(),
                ],
            )
        }
    }

    pub fn status(&self, spec: &LaunchdServiceSpec) -> Result<LaunchdStatus, LaunchdError> {
        if !self.plist_path(spec).exists() {
            return Ok(LaunchdStatus::NotInstalled);
        }
        let output = self
            .runner
            .run(&self.launchctl, &["print".to_owned(), self.target(spec)])?;
        if output.success {
            Ok(LaunchdStatus::Running)
        } else {
            Ok(LaunchdStatus::Installed)
        }
    }

    fn domain(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn target(&self, spec: &LaunchdServiceSpec) -> String {
        format!("{}/{}", self.domain(), spec.label)
    }

    fn run_launchctl(&self, operation: &'static str, args: &[String]) -> Result<(), LaunchdError> {
        let output = self.runner.run(&self.launchctl, args)?;
        if output.success {
            return Ok(());
        }
        Err(LaunchdError::CommandFailed {
            operation,
            message: if output.stderr.is_empty() {
                output.stdout
            } else {
                output.stderr
            },
        })
    }
}

fn append_string(xml: &mut String, key: &str, value: &str) {
    xml.push_str("<key>");
    xml.push_str(key);
    xml.push_str("</key><string>");
    xml.push_str(&escape_xml(value));
    xml.push_str("</string>\n");
}

fn append_array_string(xml: &mut String, value: &str) {
    xml.push_str("<string>");
    xml.push_str(&escape_xml(value));
    xml.push_str("</string>");
}

fn append_bool(xml: &mut String, key: &str, value: bool) {
    xml.push_str("<key>");
    xml.push_str(key);
    xml.push_str("</key>");
    xml.push_str(if value { "<true/>\n" } else { "<false/>\n" });
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct FakeRunner {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        output: CommandOutput,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _program: &Path, args: &[String]) -> Result<CommandOutput, LaunchdError> {
            self.calls.lock().unwrap().push(args.to_vec());
            Ok(self.output.clone())
        }
    }

    #[test]
    fn renders_escaped_plist_and_arguments() {
        let mut spec = LaunchdServiceSpec::new("com.example.loki", "/tmp/loki&services");
        spec.arguments.push("--label=\"test\"".to_owned());
        spec.environment
            .insert("A&B".to_owned(), "<value>".to_owned());
        let plist = spec.render_plist().unwrap();
        assert!(plist.contains("/tmp/loki&amp;services"));
        assert!(plist.contains("--label=&quot;test&quot;"));
        assert!(plist.contains("<key>A&amp;B</key><string>&lt;value&gt;</string>"));
    }

    #[test]
    fn rejects_invalid_label() {
        let spec = LaunchdServiceSpec::new("bad/label", "/tmp/loki");
        assert!(matches!(
            spec.render_plist(),
            Err(LaunchdError::InvalidSpec(_))
        ));
    }

    #[test]
    fn installs_and_reports_running_service() {
        let root = env::temp_dir().join(format!("loki-launchd-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                status: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            },
            ..Default::default()
        };
        let calls = runner.calls.clone();
        let manager = LaunchdManager::for_user(&root, 501, runner);
        let spec = LaunchdServiceSpec::new("com.example.loki", "/tmp/loki-services");
        let path = manager.install(&spec).unwrap();
        assert!(path.exists());
        assert_eq!(manager.status(&spec).unwrap(), LaunchdStatus::Running);
        assert_eq!(calls.lock().unwrap().len(), 2);
        let _ = fs::remove_dir_all(root);
    }
}
