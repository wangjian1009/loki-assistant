mod client;
mod mailbox;
mod projects;
mod protocol;
mod routes;
mod service;
mod status;
mod store;
mod worker;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::Router;
use loki_services_macros::loki_config;

use crate::modules::{
    config::{
        model::{
            ActionInteraction, CheckerMetadata, ConfigAction, ConfigDataType, ConfigItemMetadata,
            ConfigItemOptions, ConfigModuleMetadata, ConfigRemoteAccess, FieldOptions,
            LegacyDictionary, ListMetadata, PasswordBinding, StructureField, StructureMetadata,
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
    description: "提供 Slack 私信通知、会话查询和消息收发。",
    version: env!("CARGO_PKG_VERSION"),
};

// 所有工作区共用 Loki Assistant Slack App 的公开标识。
const APP_CLIENT_ID: Option<&str> = option_env!("LOKI_SLACK_CLIENT_ID");
const TEAM_NOTIFICATION_ENDPOINT: &str = "/slack/config/teams";

impl projects::SlackTeam {
    fn metadata() -> StructureMetadata {
        StructureMetadata {
            id_field: Some("team_id".into()),
            label_field: Some("name".into()),
            add_label: Some("新增工作区".into()),
            type_name: "SlackTeam".into(),
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
                    }),
                    read_only: false,
                    password_path: None,
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
                    password_path: None,
                    options: None,
                },
                StructureField {
                    key: "password_id".into(),
                    label: "应用令牌条目".into(),
                    data_type: ConfigDataType::PasswordReference,
                    required: false,
                    default_template: Some("loki-assistant/slack/{team_id}/app-token.gpg".into()),
                    checker: None,
                    read_only: false,
                    password_path: None,
                    options: Some(FieldOptions {
                        path: "/password/entries".into(),
                        items_field: "files".into(),
                    }),
                },
                StructureField {
                    key: "token".into(),
                    label: "Slack 网页会话凭据（JSON 对象，包含 token 和 d_cookie）".into(),
                    data_type: ConfigDataType::Password,
                    required: false,
                    read_only: false,
                    default_template: None,
                    checker: None,
                    password_path: Some("loki-assistant/slack/{id}/user-token.gpg".into()),
                    options: None,
                },
                StructureField {
                    key: "oauth_credential_id".into(),
                    label: "OAuth 凭据引用".into(),
                    data_type: ConfigDataType::String,
                    required: false,
                    read_only: true,
                    default_template: None,
                    checker: None,
                    password_path: None,
                    options: None,
                },
            ],
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
        requires_local_auth: true,
        actions: vec![
            ConfigAction {
                id: "check_app_token".into(),
                label: "检查已有令牌".into(),
                interaction: ActionInteraction::Request,
            },
            ConfigAction {
                id: "save_app_token".into(),
                label: "录入应用令牌".into(),
                interaction: ActionInteraction::Password {
                    binding: PasswordBinding {
                        key: "app_token".into(),
                        instance_parameter: "project".into(),
                        value_parameter: "token".into(),
                    },
                },
            },
            ConfigAction {
                id: "authorize".into(),
                label: "更换用户令牌".into(),
                interaction: ActionInteraction::Request,
            },
        ],
    },
};

#[loki_config]
fn slack_configuration() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "slack".into(),
        name: "Slack".into(),
        description: "Slack 工作区、用户令牌和应用令牌设置。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            ConfigItemMetadata {
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
            },
            ConfigItemMetadata {
                key: "app_token".into(),
                name: "项目应用级令牌".into(),
                description: "输入项目标识与应用级令牌。凭据保存到项目所配置的 pass 条目。".into(),
                data_type: ConfigDataType::Password,
                remote_access: ConfigRemoteAccess::Local,
                required: false,
                default: None,
                dictionary: Some(crate::modules::config::model::DictionaryMetadata {
                    entries: vec![],
                    allow_unknown: true,
                }),
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
        ],
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
            context.password.service(),
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
