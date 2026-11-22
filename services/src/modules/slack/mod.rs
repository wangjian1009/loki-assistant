mod client;
mod disk;
mod mailbox;
mod picker;
mod projects;
mod protocol;
mod routes;
mod service;
mod status;
mod store;
mod subscriptions;
mod worker;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use loki_services_macros::loki_config;

use crate::modules::{
    config::{
        model::{
            CheckerMetadata, ConfigDataType, ConfigItemMetadata, ConfigItemOptions,
            ConfigModuleMetadata, ConfigRemoteAccess, LegacyDictionary, ListMetadata,
            StructureField, StructureMetadata,
        },
        registry::ConfigItemRegistration,
    },
    discover::registry::ModuleRegistration,
};

pub use projects::Projects;

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static SLACK_MODULE: ModuleRegistration = ModuleRegistration {
    id: "slack",
    name: "Slack",
    description: "提供 Slack 提及和私信通知、会话查询和消息收发。",
    version: env!("CARGO_PKG_VERSION"),
};

const TEAM_NOTIFICATION_ENDPOINT: &str = "/slack/config/teams";
const CACHE_RETENTION_DAYS: &str = "cache_retention_days";
const DEFAULT_CACHE_RETENTION_DAYS: u64 = 30;

impl projects::SlackTeam {
    fn metadata() -> StructureMetadata {
        StructureMetadata {
            id_field: Some("team_id".into()),
            label_field: Some("name".into()),
            add_label: Some("新增工作区".into()),
            type_name: "SlackTeam".into(),
            show_all_fields: true,
            legacy_dictionary: Some(LegacyDictionary {
                key: "projects".into(),
                previous_id_field: Some("id".into()),
            }),
            fields: vec![
                StructureField {
                    key: "team_id".into(),
                    label: "Slack Team ID".into(),
                    data_type: ConfigDataType::Key,
                    required: true,
                    default_template: None,
                    checker: Some(CheckerMetadata {
                        pattern: "^T[A-Z0-9]{1,47}$".into(),
                        description: "请输入以 T 开头的 Slack Team ID。".into(),
                        emacs_pattern: Some(r"\`T[A-Z0-9]\{1,47\}\'".into()),
                    }),
                    read_only: false,
                    options: None,
                },
                StructureField {
                    key: "name".into(),
                    label: "本地显示名称".into(),
                    data_type: ConfigDataType::String,
                    required: false,
                    default_template: None,
                    checker: None,
                    read_only: false,
                    options: None,
                },
            ]
            .into_iter()
            .chain(
                [
                    ("team_name", "Slack 工作区名称"),
                    ("user_id", "Slack 用户 ID"),
                ]
                .into_iter()
                .map(|(key, label)| StructureField {
                    key: key.into(),
                    label: label.into(),
                    data_type: ConfigDataType::String,
                    required: false,
                    read_only: true,
                    default_template: None,
                    checker: None,
                    options: None,
                }),
            )
            .chain(
                [
                    ("token", "Slack 用户令牌（xoxc-）", None),
                    ("d_cookie", "Cookie d", Some("d_cookie".into())),
                ]
                .into_iter()
                .map(|(key, label, password_key)| StructureField {
                    key: key.into(),
                    label: label.into(),
                    data_type: ConfigDataType::Password {
                        path: "loki-assistant/slack/{key}.gpg".into(),
                        key: password_key,
                    },
                    required: false,
                    read_only: false,
                    default_template: None,
                    checker: (key == "token").then(|| CheckerMetadata {
                        pattern: client::WEB_TOKEN_PATTERN.into(),
                        description: "请输入以 xoxc- 开头的完整 Slack 网页会话令牌。".into(),
                        emacs_pattern: Some(r"\`xoxc-[A-Za-z0-9_-]\{1,4091\}\'".into()),
                    }),
                    options: None,
                }),
            )
            .collect(),
        }
    }
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static TEAM_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "slack",
    key: "teams",
    options: || ConfigItemOptions {
        structure: Some(projects::SlackTeam::metadata()),
        notification_endpoint: Some(TEAM_NOTIFICATION_ENDPOINT),
        create_fields: vec!["team_id"],
        save_timeout_seconds: Some(650),
        requires_local_auth: false,
        read_only: false,
    },
};

#[loki_config]
fn slack_configuration() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "slack".into(),
        name: "Slack".into(),
        description: "新增或重试待确认的工作区会打开配置提示页。可在提示页或 Emacs 配置界面填写同一 Slack 网页会话的用户令牌和 Cookie d。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![ConfigItemMetadata {
            key: "teams".into(),
            name: "Slack 工作区".into(),
            description: "每个工作区保存固定的 Team ID 和本地显示名称。".into(),
            data_type: ConfigDataType::List,
            remote_access: ConfigRemoteAccess::Local,
            required: false,
            default: Some(serde_json::json!([])),
            dictionary: None,
            list: Some(ListMetadata {
                item_type: ConfigDataType::Structure,
                min_items: None,
                max_items: None,
            }),
            checker: None,
            file_path: None,
            executable: None,
        }, ConfigItemMetadata {
            key: CACHE_RETENTION_DAYS.into(),
            name: "缓存保留天数".into(),
            description: "Slack 消息和附件从最后写入起保留的天数。".into(),
            data_type: ConfigDataType::Integer,
            remote_access: ConfigRemoteAccess::Local,
            required: false,
            default: Some(serde_json::json!(DEFAULT_CACHE_RETENTION_DAYS)),
            dictionary: None,
            list: None,
            checker: None,
            file_path: None,
            executable: None,
        }],
    }
}

#[derive(Debug)]
struct Module(Arc<Projects>);

#[linkme::distributed_slice(crate::services::EXTENSIONS)]
static EXTENSION: crate::services::ExtensionRegistration = crate::services::ExtensionRegistration {
    build: |context| {
        Arc::new(Module(Arc::new(Projects::new(
            Arc::clone(context.config),
            Arc::clone(context.notification),
            Arc::clone(context.status),
            context.emacs.service(),
        ))))
    },
};

impl crate::services::ServiceExtension for Module {
    fn config_handler(
        &self,
    ) -> Option<(
        &'static str,
        Arc<dyn crate::modules::config::service::ConfigChangeHandler>,
    )> {
        Some(("slack", self.0.clone()))
    }

    fn router(&self) -> Router {
        self.0.router()
    }

    fn config_notification_router(&self) -> Router {
        routes::config_notification_router(Arc::clone(&self.0))
    }

    fn run(self: Arc<Self>, port: u16) -> crate::services::ExtensionFuture<'static> {
        Box::pin(Arc::clone(&self.0).run(port))
    }

    fn stop(&self) -> crate::services::ExtensionFuture<'_> {
        Box::pin(self.0.stop())
    }
}
