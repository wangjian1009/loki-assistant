use std::{collections::HashMap, fmt::Debug, future::Future, pin::Pin, sync::Arc, time::Duration};

use serde::Deserialize;
use serde_json::Value;
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
    time::timeout,
};

use crate::modules::config::service::{ConfigService, ConfigServiceError};
use crate::platform::macos::notification::{
    MacNotificationLauncher, NotificationContent, NotificationLauncher,
};

use super::{
    DEFAULT_EMACSCLIENT,
    protocol::{Buffer, Notification},
};

pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, EmacsError>> + Send + 'a>>;

/// 进程边界可注入，测试不依赖真实 Emacs server。
pub trait CommandRunner: Debug + Send + Sync {
    fn run<'a>(&'a self, program: &'a str, args: &'a [String]) -> ServiceFuture<'a, CommandOutput>;
}

#[derive(Debug)]
pub struct CommandOutput {
    pub success: bool,
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run<'a>(&'a self, program: &'a str, args: &'a [String]) -> ServiceFuture<'a, CommandOutput> {
        Box::pin(async move {
            let output = Command::new(program)
                .args(args)
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|error| EmacsError::Process(error.to_string()))?;
            Ok(CommandOutput {
                success: output.status.success(),
                status: output.status.code(),
                stdout: String::from_utf8(output.stdout)
                    .map_err(|error| EmacsError::InvalidResponse(error.to_string()))?,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            })
        })
    }
}

pub trait NotificationHandler: Send + Sync {
    fn handle(&self, notification: Notification) -> ServiceFuture<'_, Value>;
}

impl<F, Fut> NotificationHandler for F
where
    F: Fn(Notification) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Value, EmacsError>> + Send + 'static,
{
    fn handle(&self, notification: Notification) -> ServiceFuture<'_, Value> {
        Box::pin(self(notification))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmacsError {
    #[error("请求无效：{0}")]
    InvalidRequest(String),
    #[error("Emacs 配置无效：{0}")]
    Config(String),
    #[error("执行 emacsclient 失败：{0}")]
    Process(String),
    #[error("emacsclient 退出码 {status:?}：{message}")]
    CommandFailed {
        status: Option<i32>,
        message: String,
    },
    #[error("Emacs 调用超时；已经开始的 Lisp 求值可能仍会继续")]
    Timeout,
    #[error("Emacs Lisp 执行失败：{0}")]
    Lisp(String),
    #[error("Emacs 返回了无效响应：{0}")]
    InvalidResponse(String),
    #[error("未注册通知处理器：{0}")]
    HandlerNotFound(String),
    #[error("通知处理器已经注册：{0}")]
    HandlerAlreadyRegistered(String),
    #[error("通知处理失败：{0}")]
    Notification(String),
}

pub struct EmacsService {
    config: Arc<ConfigService>,
    runner: Arc<dyn CommandRunner>,
    notification_launcher: Arc<dyn NotificationLauncher>,
    timeout: Duration,
    evaluation: Mutex<()>,
    handlers: RwLock<HashMap<String, Arc<dyn NotificationHandler>>>,
}

impl Debug for EmacsService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmacsService")
            .field("runner", &self.runner)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl EmacsService {
    pub fn new(config: Arc<ConfigService>) -> Self {
        Self::with_runner(
            config,
            Arc::new(ProcessCommandRunner),
            Duration::from_secs(30),
        )
    }

    pub fn with_runner(
        config: Arc<ConfigService>,
        runner: Arc<dyn CommandRunner>,
        timeout: Duration,
    ) -> Self {
        Self {
            config,
            runner,
            notification_launcher: Arc::new(MacNotificationLauncher),
            timeout,
            evaluation: Mutex::new(()),
            handlers: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_notification_launcher(mut self, launcher: Arc<dyn NotificationLauncher>) -> Self {
        self.notification_launcher = launcher;
        self
    }

    pub async fn eval(&self, script: &str) -> Result<String, EmacsError> {
        if script.trim().is_empty() {
            return Err(EmacsError::InvalidRequest("script 不能为空".to_owned()));
        }
        // 同一 gateway 串行连接，避免多个请求同时触发 daemon 启动。
        let _guard = self.evaluation.lock().await;
        let program = match self.config.value("emacs", "emacsclient_path").await {
            Ok(value) => value
                .as_str()
                .filter(|path| !path.trim().is_empty() && !path.contains('\0'))
                .ok_or_else(|| EmacsError::Config("emacsclient_path 必须是非空路径".to_owned()))?
                .to_owned(),
            Err(ConfigServiceError::ValueNotSet { .. }) => DEFAULT_EMACSCLIENT.to_owned(),
            Err(error) => return Err(EmacsError::Config(error.to_string())),
        };
        let args = vec![
            "--alternate-editor=".to_owned(),
            "--eval".to_owned(),
            eval_expression(script),
        ];
        let output = timeout(self.timeout, self.runner.run(&program, &args))
            .await
            .map_err(|_| EmacsError::Timeout)??;
        if !output.success {
            return Err(EmacsError::CommandFailed {
                status: output.status,
                message: if output.stderr.trim().is_empty() {
                    output.stdout
                } else {
                    output.stderr
                },
            });
        }
        decode_response(&output.stdout)
    }

    pub async fn buffers(&self) -> Result<Vec<Buffer>, EmacsError> {
        let result = self.eval(BUFFERS_SCRIPT).await?;
        let json: String = serde_json::from_str(&result)
            .map_err(|error| EmacsError::InvalidResponse(error.to_string()))?;
        serde_json::from_str(&json).map_err(|error| EmacsError::InvalidResponse(error.to_string()))
    }

    pub async fn register_notification_handler(
        &self,
        module_id: impl Into<String>,
        handler: Arc<dyn NotificationHandler>,
    ) -> Result<(), EmacsError> {
        let module_id = module_id.into();
        if module_id.trim().is_empty() {
            return Err(EmacsError::InvalidRequest("module_id 不能为空".to_owned()));
        }
        let mut handlers = self.handlers.write().await;
        if handlers.contains_key(&module_id) {
            return Err(EmacsError::HandlerAlreadyRegistered(module_id));
        }
        handlers.insert(module_id, handler);
        Ok(())
    }

    pub async fn unregister_notification_handler(&self, module_id: &str) -> bool {
        self.handlers.write().await.remove(module_id).is_some()
    }

    pub async fn notify(&self, notification: Notification) -> Result<Value, EmacsError> {
        if notification.module_id.trim().is_empty()
            || notification.title.trim().is_empty()
            || notification.buffer.trim().is_empty()
        {
            return Err(EmacsError::InvalidRequest(
                "module_id、title 和 buffer 不能为空".to_owned(),
            ));
        }
        let handler = self
            .handlers
            .read()
            .await
            .get(&notification.module_id)
            .cloned();
        // 不持有注册表锁执行模块代码，处理器可以继续调用 gateway 或修改注册表。
        if let Some(handler) = handler {
            timeout(self.timeout, handler.handle(notification))
                .await
                .map_err(|_| EmacsError::Timeout)?
        } else if notification.module_id == "emacs" {
            timeout(
                self.timeout,
                self.notification_launcher.launch(NotificationContent {
                    title: notification.title,
                    icon: notification.icon,
                    buffer: notification.buffer,
                }),
            )
            .await
            .map_err(|_| EmacsError::Timeout)?
            .map_err(EmacsError::Notification)?;
            Ok(
                serde_json::json!({"display": "macos", "status": "forwarded", "activation": "emacs"}),
            )
        } else {
            Err(EmacsError::HandlerNotFound(notification.module_id))
        }
    }
}

#[derive(Deserialize)]
struct EvalEnvelope {
    result: Option<String>,
    error: Option<String>,
}

fn decode_response(stdout: &str) -> Result<String, EmacsError> {
    // 自动启动 daemon 时，stdout 可能先包含 Emacs 初始化消息。
    let json = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<String>(line.trim()).ok())
        .ok_or_else(|| EmacsError::InvalidResponse("未收到 JSON 字符串响应".to_owned()))?;
    let envelope: EvalEnvelope = serde_json::from_str(&json)
        .map_err(|error| EmacsError::InvalidResponse(error.to_string()))?;
    match (envelope.result, envelope.error) {
        (_, Some(error)) => Err(EmacsError::Lisp(error)),
        (Some(result), None) => Ok(result),
        _ => Err(EmacsError::InvalidResponse(
            "响应缺少 result 或 error".to_owned(),
        )),
    }
}

fn eval_expression(script: &str) -> String {
    let source = serde_json::to_string(&format!("(progn\n{script}\n)")).expect("字符串可以序列化");
    format!(
        r#"(progn
  (require 'json)
  (condition-case err
      (let* ((source {source})
             (form (read-from-string source))
             (print-length nil)
             (print-level nil)
             (print-circle t))
        (unless (= (cdr form) (length source))
          (error "Lisp 脚本包含多余的闭括号或尾部内容"))
        (let ((result (prin1-to-string (eval (car form) t))))
          (let ((json-encoding-pretty-print nil))
            (json-encode `((result . ,result))))))
    ((error quit)
     (let ((json-encoding-pretty-print nil))
       (json-encode `((error . ,(error-message-string err))))))))"#
    )
}

const BUFFERS_SCRIPT: &str = r#"(let ((json-encoding-pretty-print nil)
      (json-null nil)
      (json-false :json-false))
 (json-encode
 (vconcat
  (mapcar (lambda (buffer)
            (with-current-buffer buffer
              `((name . ,(buffer-name))
                (file . ,buffer-file-name)
                (modified . ,(if (buffer-modified-p) t :json-false))
                (major_mode . ,(symbol-name major-mode)))))
          (buffer-list)))))"#;
