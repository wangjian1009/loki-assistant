use crate::process::Process;

use std::{io, path::PathBuf, sync::Arc, time::Duration};

use axum::http::Uri;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::{
    sync::Notify,
    time::{interval, timeout},
};
use uuid::Uuid;

use super::{
    protocol::{
        NotificationReceipt, NotificationRequest, NotificationStatusResponse, is_valid_app,
    },
    tracker::{DeliveryAttempt, NotificationTracker},
};
use crate::platform::macos::notification::{
    MacNotificationLauncher, NotificationContent, NotificationLauncher,
};

#[derive(Clone, Debug, thiserror::Error)]
pub enum NotificationError {
    #[error("请求无效：{0}")]
    InvalidRequest(String),
    #[error("无法更新通知缓存：{0}")]
    Storage(String),
    #[error("通知不存在或确认记录已过期")]
    NotFound,
    #[error("通知已收到不同的处理结果")]
    ReceiptConflict,
    #[error("通知记录已满，请稍后重试")]
    Capacity,
}

impl From<io::Error> for NotificationError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct NotificationService {
    process: Process<State>,
    retry_requested: Arc<Notify>,
}

struct State {
    launcher: Arc<dyn NotificationLauncher>,
    timeout: Duration,
    tracker: NotificationTracker,
}

impl Default for NotificationService {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationService {
    pub fn new() -> Self {
        Self::with_launcher(Arc::new(MacNotificationLauncher), Duration::from_secs(30))
    }

    pub fn load(directory: PathBuf) -> io::Result<Self> {
        Self::load_with_launcher(
            directory,
            Arc::new(MacNotificationLauncher),
            Duration::from_secs(30),
        )
    }

    pub(super) fn load_with_launcher(
        directory: PathBuf,
        launcher: Arc<dyn NotificationLauncher>,
        timeout: Duration,
    ) -> io::Result<Self> {
        Ok(Self::with_tracker(
            NotificationTracker::load(directory)?,
            launcher,
            timeout,
        ))
    }

    pub fn with_launcher(launcher: Arc<dyn NotificationLauncher>, timeout: Duration) -> Self {
        Self::with_tracker(NotificationTracker::default(), launcher, timeout)
    }

    fn with_tracker(
        tracker: NotificationTracker,
        launcher: Arc<dyn NotificationLauncher>,
        timeout: Duration,
    ) -> Self {
        Self {
            process: Process::start(State {
                launcher,
                timeout,
                tracker,
            }),
            retry_requested: Arc::new(Notify::new()),
        }
    }

    /// 在后台提交通知，并沿用通用发送流程与回执重试。
    pub(crate) fn trigger(&self, request: NotificationRequest) {
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.send(request).await {
                eprintln!("提交后台通知失败：{error}");
            }
        });
    }

    pub async fn send(
        &self,
        request: NotificationRequest,
    ) -> Result<NotificationStatusResponse, NotificationError> {
        if request.title.trim().is_empty() {
            return Err(NotificationError::InvalidRequest(
                "title 不能为空".to_owned(),
            ));
        }
        if request.app.as_deref().is_some_and(|app| !is_valid_app(app)) {
            return Err(NotificationError::InvalidRequest(
                "app 必须是仅含小写字母、数字、- 或 _ 的非空应用标识".to_owned(),
            ));
        }
        let navigation_url = request
            .callback_url
            .as_deref()
            .map(Self::navigation_url)
            .transpose()?;
        let id = Uuid::new_v4();
        let content = NotificationContent {
            notification_id: id.to_string(),
            title: request.title,
            body: request.body,
            app: request.app,
            navigation_url,
        };
        let delivered = self
            .process
            .blocking(move |state| {
                let attempt = state.tracker.register(content, id)?;
                Ok::<_, NotificationError>(state.forward(attempt))
            })
            .await?;
        let _ = delivered.await;
        self.status(&id.to_string()).await
    }

    pub(super) async fn request_pending_retry(&self) {
        self.process
            .call(|state| state.tracker.schedule_retry())
            .await;
        self.retry_requested.notify_one();
    }

    pub async fn retry_pending(&self) {
        let mut ticks = interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = self.retry_requested.notified() => {}
                _ = ticks.tick() => {}
            }
            for id in self.process.call(|state| state.tracker.due()).await {
                if let Some(delivered) = self
                    .process
                    .call(move |state| {
                        state
                            .tracker
                            .retry(id)
                            .map(|attempt| state.forward(attempt))
                    })
                    .await
                {
                    let _ = delivered.await;
                }
            }
        }
    }

    pub async fn status(&self, id: &str) -> Result<NotificationStatusResponse, NotificationError> {
        let id = id.to_owned();
        self.process.call(move |state| state.tracker.get(&id)).await
    }

    pub async fn receive_receipt(
        &self,
        id: &str,
        receipt: NotificationReceipt,
    ) -> Result<NotificationStatusResponse, NotificationError> {
        let id = id.to_owned();
        self.process
            .blocking(move |state| state.tracker.receive(&id, receipt))
            .await
    }

    fn navigation_url(callback: &str) -> Result<String, NotificationError> {
        Self::validate_callback_url(callback)?;
        Ok(format!(
            "loki-assistant://notification/activate?callback={}",
            utf8_percent_encode(callback, NON_ALPHANUMERIC)
        ))
    }

    pub(crate) fn validate_callback_url(callback: &str) -> Result<(), NotificationError> {
        let uri = callback.parse::<Uri>().map_err(|_| {
            NotificationError::InvalidRequest("回调必须是本机服务的绝对路径".to_owned())
        })?;
        if !callback.starts_with('/')
            || callback.starts_with("//")
            || uri.scheme().is_some()
            || uri.authority().is_some()
            || callback.contains(['\\', '#'])
        {
            return Err(NotificationError::InvalidRequest(
                "回调必须是本机服务的绝对路径，不接受其他主机".to_owned(),
            ));
        }
        Ok(())
    }
}

impl State {
    fn forward(&self, attempt: DeliveryAttempt) -> tokio::task::JoinHandle<()> {
        let launcher = Arc::clone(&self.launcher);
        let deadline = self.timeout;
        tokio::spawn(async move {
            let id = attempt.content.notification_id.clone();
            if !matches!(
                timeout(deadline, launcher.launch(attempt.content)).await,
                Ok(Ok(()))
            ) {
                eprintln!("通知 {id} 暂未转交给菜单栏应用，稍后重试");
            }
        })
    }
}
