use crate::process::Process;

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;

use super::{ACTIVE_KEY, Alert, AlertInput, MODULE_ID};
use crate::modules::{
    notification::{
        NotificationService,
        protocol::{NotificationRequest, is_valid_app},
    },
    status::{StatusError, StatusService},
};

#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("无法发布活跃告警状态")]
    Status(#[from] StatusError),
}

#[derive(Debug)]
pub struct AlertService {
    process: Process<State>,
}

#[derive(Debug)]
struct State {
    active: BTreeMap<String, Alert>,
    status: Arc<StatusService>,
    notification: Arc<NotificationService>,
}

impl AlertService {
    pub(crate) fn new(status: Arc<StatusService>, notification: Arc<NotificationService>) -> Self {
        status.post(MODULE_ID, ACTIVE_KEY, json!([]));
        Self {
            process: Process::start(State {
                active: BTreeMap::new(),
                status,
                notification,
            }),
        }
    }

    pub async fn publish(&self, id: &str, input: AlertInput) -> Result<Alert, AlertError> {
        let (alert, _) = self.publish_with_creation(id, input).await?;
        Ok(alert)
    }

    pub(crate) async fn publish_silent(
        &self,
        id: &str,
        input: AlertInput,
    ) -> Result<Alert, AlertError> {
        let (alert, _) = self.publish_with_notification(id, input, false).await?;
        Ok(alert)
    }

    pub(super) async fn publish_with_creation(
        &self,
        id: &str,
        input: AlertInput,
    ) -> Result<(Alert, bool), AlertError> {
        self.publish_with_notification(id, input, true).await
    }

    async fn publish_with_notification(
        &self,
        id: &str,
        input: AlertInput,
        notify: bool,
    ) -> Result<(Alert, bool), AlertError> {
        validate_id(id)?;
        if input.title.trim().is_empty() {
            return Err(AlertError::InvalidRequest("title 不能为空"));
        }
        if input.app.as_deref().is_some_and(|app| !is_valid_app(app)) {
            return Err(AlertError::InvalidRequest(
                "app 必须是仅含小写字母、数字、- 或 _ 的非空应用标识",
            ));
        }
        if let Some(callback) = input.callback_url.as_deref() {
            NotificationService::validate_callback_url(callback)
                .map_err(|_| AlertError::InvalidRequest("callback_url 必须是本机服务的绝对路径"))?;
        }

        let id = id.to_owned();
        self.process
            .with(move |state| Box::pin(async move { state.publish(&id, input, notify).await }))
            .await
    }

    pub async fn clear(&self, id: &str) -> Result<bool, AlertError> {
        validate_id(id)?;
        let id = id.to_owned();
        self.process
            .with(move |state| Box::pin(async move { state.clear(&id).await }))
            .await
    }
}

impl State {
    async fn publish(
        &mut self,
        id: &str,
        input: AlertInput,
        notify: bool,
    ) -> Result<(Alert, bool), AlertError> {
        let previous = self.active.get(id);
        if let Some(previous) = previous.filter(|alert| {
            alert.app == input.app
                && alert.title == input.title
                && alert.body == input.body
                && alert.callback_url == input.callback_url
                && alert.level == input.level
        }) {
            return Ok((previous.clone(), false));
        }
        let created = previous.is_none();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let alert = Alert {
            id: id.to_owned(),
            app: input.app,
            title: input.title,
            body: input.body,
            callback_url: input.callback_url,
            level: input.level,
            created_at: previous.map_or(now, |alert| alert.created_at),
            updated_at: now,
        };
        let mut next = self.active.clone();
        next.insert(id.to_owned(), alert.clone());
        self.publish_snapshot(&next).await?;
        self.active = next;

        if notify {
            self.notification.trigger(NotificationRequest {
                app: alert.app.clone(),
                title: alert.title.clone(),
                body: alert.body.clone(),
                callback_url: alert.callback_url.clone(),
            });
        }
        Ok((alert, created))
    }

    async fn clear(&mut self, id: &str) -> Result<bool, AlertError> {
        if !self.active.contains_key(id) {
            return Ok(false);
        }
        let mut next = self.active.clone();
        next.remove(id);
        self.publish_snapshot(&next).await?;
        self.active = next;
        Ok(true)
    }

    async fn publish_snapshot(&self, active: &BTreeMap<String, Alert>) -> Result<(), StatusError> {
        let alerts = json!(active.values().collect::<Vec<_>>());
        self.status
            .publish_many_validated(BTreeMap::from([(
                MODULE_ID.to_owned(),
                BTreeMap::from([(ACTIVE_KEY.to_owned(), alerts)]),
            )]))
            .await?;
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<(), AlertError> {
    let mut bytes = id.bytes();
    if id.len() > 128
        || !bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AlertError::InvalidRequest(
            "id 必须包含 1–128 个 ASCII 字母、数字、.、_ 或 -，并以字母或数字开头",
        ));
    }
    Ok(())
}
