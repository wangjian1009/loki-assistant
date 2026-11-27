mod diagnostic;
mod progress;
mod routes;
mod status;
mod sync;

use std::{path::PathBuf, sync::Arc};

use axum::Router;
use loki_services_macros::loki_config;
use serde_json::json;
use tokio::sync::Notify;

use crate::{
    modules::{
        alert::AlertService,
        config::{
            model::{ConfigDataType, ConfigItemMetadata, ConfigModuleMetadata, ConfigRemoteAccess},
            service::{ConfigService, ConfigServiceError},
        },
        discover::registry::ModuleRegistration,
        emacs::service::EmacsService,
        notification::NotificationService,
        status::StatusService,
    },
    services::{EXTENSIONS, ExtensionFuture, ExtensionRegistration, ServiceExtension},
};

const MODULE_ID: &str = "mail";
const CONFIG_PATH: &str = "mbsync_config";
const ENABLED: &str = "enabled";
const UNREAD_QUERY: &str = "maildir:/inbox/ AND flag:unread AND NOT flag:trashed";

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static MAIL_MODULE: ModuleRegistration = ModuleRegistration {
    id: MODULE_ID,
    name: "邮件",
    description: "同步 Maildir、索引新邮件，并在 Emacs 中打开邮件或撰写草稿。",
    version: env!("CARGO_PKG_VERSION"),
};

#[loki_config]
fn mail_configuration() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: MODULE_ID.into(),
        name: "邮件".into(),
        description: "启用后每五分钟同步 mbsync 的 loki-auto 组，并更新 mu 索引。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: [
            (
                ENABLED,
                "自动同步",
                "关闭后停止自动同步。",
                ConfigDataType::Boolean,
                json!(false),
            ),
            (
                CONFIG_PATH,
                "mbsync 配置文件",
                "指定绝对路径或 ~/ 开头的路径。同步失败告警会打开此文件。",
                ConfigDataType::String,
                json!("~/.mbsyncrc"),
            ),
        ]
        .into_iter()
        .map(
            |(key, name, description, data_type, default)| ConfigItemMetadata {
                key: key.into(),
                name: name.into(),
                description: description.into(),
                data_type,
                remote_access: ConfigRemoteAccess::Local,
                required: true,
                default: Some(default),
                dictionary: None,
                list: None,
                checker: (key == CONFIG_PATH).then(|| {
                    crate::modules::config::model::CheckerMetadata {
                        pattern: r"^(?:/|~/)[^\x00\r\n]+$".into(),
                        description: "请输入绝对路径或 ~/ 开头的路径。".into(),
                        emacs_pattern: None,
                    }
                }),
                file_path: None,
                executable: None,
            },
        )
        .collect(),
    }
}

#[derive(Debug)]
struct MailService {
    config: Arc<ConfigService>,
    alert: Arc<AlertService>,
    notification: Arc<NotificationService>,
    emacs: Arc<EmacsService>,
    status: Arc<StatusService>,
    sync_progress: progress::SyncProgress,
    refresh_requested: Notify,
    stopping: Notify,
}

#[derive(Debug)]
struct MailModule(Arc<MailService>);

#[linkme::distributed_slice(EXTENSIONS)]
static EXTENSION: ExtensionRegistration = ExtensionRegistration {
    build: |context| {
        Arc::new(MailModule(Arc::new(MailService {
            config: Arc::clone(context.config),
            alert: Arc::clone(context.alert),
            notification: Arc::clone(context.notification),
            emacs: context.emacs.service(),
            status: Arc::clone(context.status),
            sync_progress: progress::SyncProgress::new(Arc::clone(context.status)),
            refresh_requested: Notify::new(),
            stopping: Notify::new(),
        })))
    },
};

impl MailService {
    async fn config_path(&self) -> Result<PathBuf, ConfigServiceError> {
        let value = self.config.value(MODULE_ID, CONFIG_PATH).await?;
        let path = value.value.as_str().unwrap_or_default();
        if let Some(relative) = path.strip_prefix("~/") {
            return std::env::home_dir()
                .map(|home| home.join(relative))
                .ok_or_else(|| ConfigServiceError::InvalidValue("无法定位用户目录".into()));
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(ConfigServiceError::InvalidValue(
                "mbsync 配置路径无效".into(),
            ));
        }
        Ok(path)
    }
}

impl ServiceExtension for MailModule {
    fn router(&self) -> Router {
        crate::http::mount_routes(Router::new(), MODULE_ID).with_state(Arc::clone(&self.0))
    }

    fn run(self: Arc<Self>, _port: u16) -> ExtensionFuture<'static> {
        Box::pin(async move {
            tokio::select! {
                biased;
                _ = self.0.stopping.notified() => {},
                _ = self.0.sync_loop() => {},
            }
        })
    }

    fn stop(&self) -> ExtensionFuture<'_> {
        Box::pin(async { self.0.stopping.notify_one() })
    }
}
