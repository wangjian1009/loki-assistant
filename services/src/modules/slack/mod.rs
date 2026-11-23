mod client;
mod disk;
mod downloads;
mod mailbox;
mod notification;
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

use std::{fs, io, path::Path, sync::Arc};

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

pub(crate) fn migrate_legacy_cache(cache: &Path) -> io::Result<()> {
    let legacy = cache.join("slack");
    let target = cache.join("assistant/slack");
    let metadata = match fs::symlink_metadata(&legacy) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !target.try_exists()? {
        fs::create_dir_all(cache.join("assistant"))?;
        fs::rename(legacy, target)?;
    }
    Ok(())
}

#[linkme::distributed_slice(crate::modules::discover::registry::MODULE_DESCRIPTORS)]
static SLACK_MODULE: ModuleRegistration = ModuleRegistration {
    id: "slack",
    name: "Slack",
    description: "提供 Slack 提及、私信和主题关注通知、会话查询和消息收发。",
    version: env!("CARGO_PKG_VERSION"),
};

const TEAM_NOTIFICATION_ENDPOINT: &str = "/slack/config/teams";
const NICKNAMES: &str = "nicknames";
const WATCHES: &str = "watches";
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

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static WATCH_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "slack",
    key: WATCHES,
    options: || ConfigItemOptions {
        read_only: true,
        structure: Some(StructureMetadata {
            id_field: Some("id".into()),
            label_field: Some("display_name".into()),
            add_label: None,
            legacy_dictionary: None,
            type_name: "SlackWatch".into(),
            show_all_fields: false,
            fields: [
                ("id", "会话 ID", ConfigDataType::Key),
                ("display_name", "显示名称", ConfigDataType::String),
            ]
            .into_iter()
            .map(|(key, label, data_type)| StructureField {
                key: key.into(),
                label: label.into(),
                data_type,
                required: true,
                default_template: None,
                checker: None,
                read_only: false,
                options: None,
            })
            .collect(),
        }),
        ..ConfigItemOptions::default()
    },
};

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static NICKNAME_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "slack",
    key: NICKNAMES,
    options: || ConfigItemOptions {
        read_only: true,
        structure: Some(StructureMetadata {
            id_field: Some("id".into()),
            label_field: Some("nickname".into()),
            add_label: None,
            legacy_dictionary: None,
            type_name: "SlackNickname".into(),
            show_all_fields: false,
            fields: [
                ("id", "用户 ID", ConfigDataType::Key),
                ("nickname", "昵称", ConfigDataType::String),
            ]
            .into_iter()
            .map(|(key, label, data_type)| StructureField {
                key: key.into(),
                label: label.into(),
                data_type,
                required: true,
                default_template: None,
                checker: None,
                read_only: false,
                options: None,
            })
            .collect(),
        }),
        ..ConfigItemOptions::default()
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
            key: WATCHES.into(),
            name: "关注的主题聊天".into(),
            description: "由 Slack 模块通过主题聊天的关注操作维护。".into(),
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
            key: NICKNAMES.into(),
            name: "用户昵称".into(),
            description: "按 Slack 工作区和用户 ID 保存的本机昵称。".into(),
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
            Arc::clone(context.alert),
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
