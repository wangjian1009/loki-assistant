use std::{sync::Arc, time::Duration};

use axum::http::Uri;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::time::timeout;

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
    #[error("系统通知转交失败：{0}")]
    Launch(String),
    #[error("系统通知转交超时，处理结果未知")]
    Timeout,
    #[error("通知不存在、已过期或 services 已重启")]
    NotFound,
    #[error("通知已收到不同的处理结果")]
    ReceiptConflict,
    #[error("通知记录已满，请稍后重试")]
    Capacity,
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
        let notification_id = self.tracker.register().await?;
        let content = NotificationContent {
            notification_id: notification_id.clone(),
            title: request.title,
            body: request.body,
            app: request.app,
            navigation_url,
        };
        let result = timeout(self.timeout, self.launcher.launch(content))
            .await
            .map_err(|_| NotificationError::Timeout)
            .and_then(|result| result.map_err(NotificationError::Launch));
        if let Err(error) = result {
            self.tracker.remove(&notification_id).await;
            return Err(error);
        }
        self.tracker.get(&notification_id).await
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
