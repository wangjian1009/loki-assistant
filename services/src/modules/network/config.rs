use axum::{
    Extension, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use loki_services_macros::loki_config;
use serde_json::Value;
use std::sync::Arc;

use crate::modules::config::{
    model::{ConfigDataType, ConfigItemMetadata, ConfigItemOptions, ConfigModuleMetadata},
    registry::ConfigItemRegistration,
    service::{ConfigService, ConfigServiceError, NotificationRequest, Password},
};

const RELAY_NOTIFICATION: &str = "/network/config/relay-url";

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static RELAY_URL_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "network",
    key: "relay_url",
    options: || ConfigItemOptions {
        monitor: Some(crate::modules::config::model::ConfigMonitor {
            id: "relay",
            endpoint: RELAY_NOTIFICATION,
        }),
        ..ConfigItemOptions::default()
    },
};

pub(crate) fn notification_router(service: Arc<super::NetworkService>) -> Router {
    Router::new()
        .route(
            &format!("{RELAY_NOTIFICATION}/{{value}}"),
            post(notify_relay_url).delete(notify_relay_url),
        )
        .with_state(service)
}

async fn notify_relay_url(
    State(network): State<Arc<super::NetworkService>>,
    Extension(change): Extension<NotificationRequest>,
    Path(url): Path<String>,
) -> Response {
    let url = if change.deleted || url == "\0" {
        String::new()
    } else {
        url
    };
    match network.prepare_relay_edit(url).await {
        Ok(apply) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            change.settle_with(&mut response, apply);
            response
        }
        Err(message) => {
            let mut response = StatusCode::UNPROCESSABLE_ENTITY.into_response();
            response.extensions_mut().insert(
                crate::modules::config::service::NotificationValidationError {
                    code: "network-relay-registration".into(),
                    message,
                    clear_password_fields: Vec::new(),
                },
            );
            response
        }
    }
}

const RELAY_PASSWORD_PATH: &str = "loki-assistant/network/relay.gpg";

pub(super) fn relay_password() -> Password {
    Password::new(RELAY_PASSWORD_PATH.into())
}

pub(super) async fn relay_token(config: &ConfigService) -> Result<String, ConfigServiceError> {
    Ok(relay_password()
        .read_optional(config)
        .await?
        .unwrap_or_default())
}

#[cfg(test)]
pub(super) async fn relay_token_is_set(config: &ConfigService) -> bool {
    relay_password()
        .read_optional(config)
        .await
        .ok()
        .flatten()
        .is_some_and(|value| !value.trim().is_empty())
}

#[linkme::distributed_slice(crate::modules::config::registry::CONFIG_ITEMS)]
static RELAY_TOKEN_CONFIG: ConfigItemRegistration = ConfigItemRegistration {
    module_id: "network",
    key: "relay_token",
    options: || ConfigItemOptions {
        read_only: true,
        ..ConfigItemOptions::default()
    },
};

#[loki_config]
fn network_config_metadata() -> ConfigModuleMetadata {
    ConfigModuleMetadata {
        id: "network".to_owned(),
        name: "网络".to_owned(),
        description: "管理本地身份和中转服务器注册。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![
            ConfigItemMetadata {
                key: "relay_url".to_owned(),
                name: "中转地址".to_owned(),
                description:
                    "中转服务器的完整 HTTP(S) URL；地址和令牌都非空时注册，缺少任意一项视为未配置。"
                        .to_owned(),
                data_type: ConfigDataType::String,
                remote_access: crate::modules::config::model::ConfigRemoteAccess::Read,
                required: false,
                default: Some(Value::String(String::new())),
                dictionary: None,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
            ConfigItemMetadata {
                key: "relay_token".to_owned(),
                name: "中转令牌".to_owned(),
                description: "保存在本机 GPG 密码库中的共享访问令牌，与中转地址一起生效。"
                    .to_owned(),
                data_type: ConfigDataType::Password {
                    path: RELAY_PASSWORD_PATH.into(),
                    key: None,
                },
                remote_access: crate::modules::config::model::ConfigRemoteAccess::Local,
                required: false,
                default: None,
                dictionary: None,
                list: None,
                checker: None,
                file_path: None,
                executable: None,
            },
        ],
    }
}
