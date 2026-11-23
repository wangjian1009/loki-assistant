use std::{io, path::PathBuf, sync::Arc, time::Duration};

use axum::http::Uri;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::time::{interval, timeout};
use uuid::Uuid;

use super::{
    protocol::{
        NotificationReceipt, NotificationRequest, NotificationStatusResponse, is_valid_app,
    },
    tracker::NotificationTracker,
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

#[derive(Debug)]
pub struct NotificationService {
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
        Ok(Self {
            launcher,
            timeout,
            tracker: NotificationTracker::load(directory)?,
        })
    }

    pub fn with_launcher(launcher: Arc<dyn NotificationLauncher>, timeout: Duration) -> Self {
        Self {
            launcher,
            timeout,
            tracker: NotificationTracker::default(),
        }
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
        self.tracker.register(content.clone(), id).await?;
        self.forward(content).await;
        self.tracker.get(&id.to_string()).await
    }

    pub async fn retry_pending(&self) {
        let mut ticks = interval(Duration::from_secs(15));
        loop {
            ticks.tick().await;
            for id in self.tracker.due().await {
                if let Some(content) = self.tracker.retry(id).await {
                    self.forward(content).await;
                }
            }
        }
    }

    async fn forward(&self, content: NotificationContent) {
        let id = content.notification_id.clone();
        if !matches!(
            timeout(self.timeout, self.launcher.launch(content)).await,
            Ok(Ok(()))
        ) {
            eprintln!("通知 {id} 暂未转交给菜单栏应用，稍后重试");
        }
    }

    pub async fn status(&self, id: &str) -> Result<NotificationStatusResponse, NotificationError> {
        self.tracker.get(id).await
    }

    pub async fn receive_receipt(
        &self,
        id: &str,
        receipt: NotificationReceipt,
    ) -> Result<NotificationStatusResponse, NotificationError> {
        self.tracker.receive(id, receipt).await
    }

    fn navigation_url(callback: &str) -> Result<String, NotificationError> {
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
        Ok(format!(
            "loki-assistant://notification/activate?callback={}",
            utf8_percent_encode(callback, NON_ALPHANUMERIC)
        ))
    }
}
