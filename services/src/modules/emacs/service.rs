use std::{collections::HashMap, fmt::Debug, future::Future, pin::Pin, sync::Arc, time::Duration};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

use serde::Deserialize;
use serde_json::Value;
use tokio::{
    process::Command,
    sync::{Mutex, RwLock},
    time::timeout,
};

use crate::modules::status::{
    StatusService,
    protocol::{StatusResponse, StatusValues},
};

use crate::modules::notification::{
    NotificationError, NotificationService, protocol::NotificationRequest,
};

use super::{
    config::EmacsConfig,
    protocol::{
        Buffer, FullCaptureRequest, FullCaptureResult, Notification, NotificationReceipt,
        NotificationStatusResponse,
    },
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
    #[error("{0}")]
    RemoteStatusForbidden(String),
    #[error("Emacs 配置无效：{0}")]
    Config(String),
    #[error("执行 emacsclient 失败：{0}")]
    Process(String),
    #[error("无法连接目标 Emacs，请确认该实例的 server 已启动及 socket 配置正确：{0}")]
    ConnectionFailed(String),
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
    #[error(transparent)]
    SystemNotification(#[from] NotificationError),
    #[error("目标 buffer 已不存在")]
    BufferNotFound,
    #[error("Emacs 应用不存在或未安装：{0}")]
    AppNotFound(String),
    #[error("Emacs capture 失败 [{code}]：{message}")]
    Capture { code: String, message: String },
}

pub struct EmacsService {
    config: EmacsConfig,
    runner: Arc<dyn CommandRunner>,
    notifications: Arc<NotificationService>,
    status: Arc<StatusService>,
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
    pub fn new(
        config: EmacsConfig,
        notifications: Arc<NotificationService>,
        status: Arc<StatusService>,
    ) -> Self {
        Self::with_runner(
            config,
            notifications,
            status,
            Arc::new(ProcessCommandRunner),
            Duration::from_secs(30),
        )
    }

    pub fn with_runner(
        config: EmacsConfig,
        notifications: Arc<NotificationService>,
        status: Arc<StatusService>,
        runner: Arc<dyn CommandRunner>,
        timeout: Duration,
    ) -> Self {
        Self {
            config,
            runner,
            notifications,
            status,
            timeout,
            evaluation: Mutex::new(()),
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Emacs 业务入口接收上报后，通过共享 Rust 接口发布，不另建状态缓存。
    pub fn receive_status(&self, values: StatusValues) -> Result<StatusResponse, EmacsError> {
        self.receive_status_for(values, false)
    }

    pub(super) fn receive_status_for(
        &self,
        values: StatusValues,
        remote: bool,
    ) -> Result<StatusResponse, EmacsError> {
        use crate::modules::status::StatusError;
        self.status
            .publish_many_validated_for(values, remote)
            .map_err(|error| match error {
                StatusError::RemoteAccessForbidden(..) => {
                    EmacsError::RemoteStatusForbidden(error.to_string())
                }
                _ => EmacsError::InvalidRequest(error.to_string()),
            })
    }

    pub async fn eval(&self, script: &str) -> Result<String, EmacsError> {
        self.eval_with_startup(script, true).await
    }

    async fn eval_with_startup(
        &self,
        script: &str,
        auto_start: bool,
    ) -> Result<String, EmacsError> {
        if script.trim().is_empty() {
            return Err(EmacsError::InvalidRequest("script 不能为空".to_owned()));
        }
        // 同一 gateway 串行连接，避免多个请求同时触发 daemon 启动。
        let _guard = self.evaluation.lock().await;
        let program = match self.config.emacsclient_path().await {
            Ok(value) => value
                .value
                .as_str()
                .filter(|path| !path.trim().is_empty() && !path.contains('\0'))
                .ok_or_else(|| EmacsError::Config("emacsclient_path 必须是非空路径".to_owned()))?
                .to_owned(),
            Err(error) => return Err(EmacsError::Config(error.to_string())),
        };
        let socket = self
            .config
            .socket_name()
            .await
            .map_err(|error| EmacsError::Config(error.to_string()))?;
        let socket = socket
            .value
            .as_str()
            .filter(|name| !name.trim().is_empty() && !name.contains('\0'))
            .ok_or_else(|| {
                EmacsError::Config("socket_name 必须是非空 socket 名称或路径".to_owned())
            })?;
        let args = vec![
            if auto_start {
                "--alternate-editor="
            } else {
                "--alternate-editor=/usr/bin/false"
            }
            .to_owned(),
            // 显式指定 socket，禁止 emacsclient 在连接失败后尝试 TCP server 文件。
            format!("--socket-name={socket}"),
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

    pub async fn capture_full(
        &self,
        request: FullCaptureRequest,
    ) -> Result<FullCaptureResult, EmacsError> {
        validate_full_capture_request(&request)?;
        let request_json = serde_json::to_string(&request)
            .map_err(|error| EmacsError::InvalidRequest(error.to_string()))?;
        let argument = serde_json::to_string(&request_json)
            .map_err(|error| EmacsError::InvalidRequest(error.to_string()))?;
        let script = format!(
            r#"(progn
  (require 'json)
  (if (require 'loki-assistant-capture nil t)
      (if (fboundp 'loki-assistant-capture-full)
          (loki-assistant-capture-full {argument})
        (json-encode '((error . ((code . "capture-not-installed")
                                  (message . "Emacs 未提供 loki-assistant-capture-full。"))))))
    (json-encode '((error . ((code . "capture-not-installed")
                              (message . "Emacs 未加载 loki-assistant-capture。")))))))"#,
            argument = argument
        );
        let result = self.eval(&script).await?;
        let wire: CaptureWire = serde_json::from_str(&result)
            .map_err(|error| EmacsError::InvalidResponse(format!("capture 响应无效：{error}")))?;
        if let Some(error) = wire.error {
            return Err(EmacsError::Capture {
                code: error.code,
                message: error.message,
            });
        }
        let result = FullCaptureResult {
            status: wire
                .status
                .ok_or_else(|| EmacsError::InvalidResponse("capture 响应缺少 status".to_owned()))?,
            url: wire
                .url
                .ok_or_else(|| EmacsError::InvalidResponse("capture 响应缺少 url".to_owned()))?,
            document: wire.document,
        };
        if !matches!(result.status.as_str(), "pending" | "existing")
            || result.url.trim().is_empty()
            || result.url != request.url
            || (result.status == "existing" && result.document.is_none())
            || (result.status == "pending" && result.document.is_some())
        {
            return Err(EmacsError::InvalidResponse(
                "capture 响应的状态或文档字段无效".to_owned(),
            ));
        }
        Ok(result)
    }

    pub async fn open_app(&self, app: &str) -> Result<(), EmacsError> {
        if app.trim().is_empty() {
            return Err(EmacsError::InvalidRequest("app 不能为空".to_owned()));
        }
        let script = match app {
            "config" => OPEN_CONFIG_APP_SCRIPT,
            "logs" => OPEN_LOG_APP_SCRIPT,
            _ => return Err(EmacsError::AppNotFound(app.to_owned())),
        };
        // 应用标识只选择固定入口，不作为 Lisp 符号或脚本执行。
        match self.eval(script).await?.as_str() {
            "t" => Ok(()),
            "nil" => Err(EmacsError::AppNotFound(app.to_owned())),
            _ => Err(EmacsError::InvalidResponse(
                "打开 Emacs 应用未返回 t 或 nil".to_owned(),
            )),
        }
    }

    pub(crate) async fn open_slack_conversation(
        &self,
        target: &Value,
    ) -> Result<String, EmacsError> {
        let argument = serde_json::to_string(&target.to_string()).expect("JSON 字符串可序列化");
        let script = format!(
            "(when (require 'loki-assistant-dispatch nil t) (loki-assistant-dispatch-slack-notification {argument}))"
        );
        let output = timeout(
            Duration::from_secs(10),
            self.eval_with_startup(&script, false),
        )
        .await
        .map_err(|_| EmacsError::Timeout)?
        .map_err(|error| match error {
            EmacsError::Process(_) | EmacsError::CommandFailed { .. } => {
                EmacsError::ConnectionFailed(error.to_string())
            }
            error => error,
        })?;
        match output.as_str() {
            "conversation" | "thread" | "message" => Ok(output),
            "nil" => Err(EmacsError::AppNotFound("slack".into())),
            _ => Err(EmacsError::InvalidResponse(
                "Slack 会话跳转结果无效。".into(),
            )),
        }
    }

    pub(crate) async fn activate_slack(&self) -> Result<(), EmacsError> {
        let script = "(when (require 'loki-assistant-dispatch nil t) (loki-assistant-dispatch-activate-slack))";
        match self.eval(script).await?.as_str() {
            "t" => Ok(()),
            "nil" => Err(EmacsError::AppNotFound("slack".into())),
            _ => Err(EmacsError::InvalidResponse("Slack 聊天激活回执无效。".into())),
        }
    }

    pub(crate) async fn dispatch_slack_messages(&self, payload: &Value) -> Result<bool, EmacsError> {
        let path = std::env::temp_dir().join(format!("loki-slack-{}.json", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| EmacsError::InvalidResponse("无法创建 Slack 投递文件。".into()))?;
        let path = SlackPayloadFile(path);
        serde_json::to_writer(&mut file, payload)
            .and_then(|_| file.flush().map_err(serde_json::Error::io))
            .map_err(|_| EmacsError::InvalidResponse("无法写入 Slack 投递文件。".into()))?;
        drop(file);
        let argument = serde_json::to_string(&path.0.to_string_lossy())
            .expect("投递文件路径可序列化");
        let script = format!(
            "(if (require 'loki-assistant-dispatch nil t) \
                 (loki-assistant-dispatch-slack-messages {argument}) 'buffer-closed)"
        );
        match timeout(Duration::from_secs(10), self.eval_with_startup(&script, false)).await {
            Ok(Ok(output)) if output == "applied" => Ok(true),
            Ok(Ok(output)) if output == "buffer-closed" => Ok(false),
            Ok(Err(EmacsError::CommandFailed { .. } | EmacsError::ConnectionFailed(_))) => Ok(false),
            Ok(Ok(_)) => Err(EmacsError::InvalidResponse("Slack 消息分发回执无效。".into())),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(EmacsError::Timeout),
        }
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
        if notification
            .app
            .as_deref()
            .is_some_and(|app| !crate::modules::notification::protocol::is_valid_app(app))
        {
            return Err(EmacsError::InvalidRequest(
                "app 必须是仅含小写字母、数字、- 或 _ 的非空应用标识".to_owned(),
            ));
        }
        if notification.module_id.trim().is_empty()
            || notification.title.trim().is_empty()
            || notification
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.trim().is_empty())
        {
            return Err(EmacsError::InvalidRequest(
                "module_id、title 不能为空；提供 buffer 时必须为非空字符串".to_owned(),
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
            let callback_url = match &notification.buffer {
                Some(buffer) => format!(
                    "/emacs/activate?buffer={}",
                    percent_encoding::utf8_percent_encode(
                        buffer,
                        percent_encoding::NON_ALPHANUMERIC
                    )
                ),
                None => "/emacs/activate".to_owned(),
            };
            let result = self
                .notifications
                .send(NotificationRequest {
                    title: notification.title,
                    body: notification
                        .body
                        .or_else(|| Some("点击显示 Emacs".to_owned())),
                    app: notification.app.or_else(|| Some("emacs".to_owned())),
                    callback_url: Some(callback_url),
                })
                .await?;
            Ok(
                serde_json::json!({"display":"macos", "status":result.status, "activation":"emacs", "notification_id":result.notification_id}),
            )
        } else {
            Err(EmacsError::HandlerNotFound(notification.module_id))
        }
    }

    pub async fn activate(&self, buffer: Option<&str>) -> Result<(), EmacsError> {
        if let Some(buffer) = buffer {
            if buffer.trim().is_empty() {
                return Err(EmacsError::InvalidRequest("buffer 不能为空".to_owned()));
            }
            let result = self
                .eval_with_startup(&activation_script(buffer), false)
                .await
                .map_err(|error| match error {
                    EmacsError::Process(_) | EmacsError::CommandFailed { .. } => {
                        EmacsError::ConnectionFailed(error.to_string())
                    }
                    error => error,
                })?;
            match result.as_str() {
                "t" => {}
                "nil" => return Err(EmacsError::BufferNotFound),
                _ => {
                    return Err(EmacsError::InvalidResponse(
                        "buffer 激活未返回 t 或 nil".to_owned(),
                    ));
                }
            }
        } else {
            let args = vec!["-b".to_owned(), "org.gnu.Emacs".to_owned()];
            let output = timeout(self.timeout, self.runner.run("/usr/bin/open", &args))
                .await
                .map_err(|_| EmacsError::Timeout)??;
            if !output.success {
                return Err(EmacsError::CommandFailed {
                    status: output.status,
                    message: output.stderr,
                });
            }
        }
        Ok(())
    }

    pub async fn notification_status(
        &self,
        id: &str,
    ) -> Result<NotificationStatusResponse, EmacsError> {
        self.notifications.status(id).await.map_err(Into::into)
    }

    pub async fn receive_notification_receipt(
        &self,
        id: &str,
        receipt: NotificationReceipt,
    ) -> Result<NotificationStatusResponse, EmacsError> {
        self.notifications
            .receive_receipt(id, receipt)
            .await
            .map_err(Into::into)
    }
}

struct SlackPayloadFile(PathBuf);

impl Drop for SlackPayloadFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Debug, Deserialize)]
struct CaptureWire {
    status: Option<String>,
    url: Option<String>,
    document: Option<super::protocol::FullCaptureDocument>,
    error: Option<CaptureWireError>,
}

#[derive(Debug, Deserialize)]
struct CaptureWireError {
    code: String,
    message: String,
}

fn validate_full_capture_request(request: &FullCaptureRequest) -> Result<(), EmacsError> {
    if request.title.trim().is_empty()
        || request.title.contains('\r')
        || request.title.contains('\n')
    {
        return Err(EmacsError::InvalidRequest(
            "title 必须是非空且不含换行的字符串".to_owned(),
        ));
    }
    if request.body.trim().is_empty() {
        return Err(EmacsError::InvalidRequest(
            "body 必须是非空字符串".to_owned(),
        ));
    }
    if request.body.len() > 16 * 1024 * 1024 {
        return Err(EmacsError::InvalidRequest(
            "body 超过 16 MiB 限制".to_owned(),
        ));
    }
    if request.url.chars().any(char::is_whitespace) {
        return Err(EmacsError::InvalidRequest(
            "url 不能包含空白字符".to_owned(),
        ));
    }
    let parsed = url::Url::parse(&request.url)
        .map_err(|_| EmacsError::InvalidRequest("url 必须是有效的绝对 URL".to_owned()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(EmacsError::InvalidRequest(
            "url 必须是绝对 HTTP 或 HTTPS URL".to_owned(),
        ));
    }
    let timestamp = regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z$")
        .expect("captured_at 正则有效");
    if !timestamp.is_match(&request.captured_at) {
        return Err(EmacsError::InvalidRequest(
            "captured_at 必须是 UTC RFC 3339 时间".to_owned(),
        ));
    }
    Ok(())
}

fn activation_script(buffer: &str) -> String {
    let buffer_name = serde_json::to_string(buffer).expect("字符串可以序列化");
    activation_script_for_target(&format!("(get-buffer {buffer_name})"))
}

fn activation_script_for_target(target: &str) -> String {
    format!(
        r#"(progn
  (require 'seq)
  (let ((target {target}))
    (when (buffer-live-p target)
      (let* ((window (seq-find (lambda (candidate)
                                (display-graphic-p (window-frame candidate)))
                              (get-buffer-window-list target nil t)))
             (frame (or (and window (window-frame window))
                        (and (display-graphic-p (selected-frame)) (selected-frame))
                        (seq-find (lambda (candidate) (display-graphic-p candidate)) (frame-list))
                        (make-frame '((window-system . ns))))))
        (select-frame frame)
        (when window (select-window window))
        (switch-to-buffer target)
        (make-frame-visible frame)
        (select-frame-set-input-focus frame)
        t))))"#
    )
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

const OPEN_CONFIG_APP_SCRIPT: &str = r#"(progn
  (require 'seq)
  (when (require 'loki-assistant-config nil t)
    (let ((frame (or (and (display-graphic-p (selected-frame)) (selected-frame))
                     (seq-find #'display-graphic-p (frame-list))
                     (make-frame '((window-system . ns))))))
      (select-frame frame)
      (loki-assistant-config)
      (make-frame-visible frame)
      (select-frame-set-input-focus frame)
      t)))"#;

const OPEN_LOG_APP_SCRIPT: &str = r#"(progn
  (require 'seq)
  (let* ((path (expand-file-name "~/.assistant/loki-assistant.log.org"))
         (frame (or (and (display-graphic-p (selected-frame)) (selected-frame))
                    (seq-find #'display-graphic-p (frame-list))
                    (make-frame '((window-system . ns)))))
         (buffer (find-file path)))
    (select-frame frame)
    (switch-to-buffer buffer)
    (make-frame-visible frame)
    (select-frame-set-input-focus frame)
    t))"#;

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
