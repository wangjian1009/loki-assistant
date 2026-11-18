use super::protocol::ConnectionStatus;
use crate::modules::status::model::{
    StatusChoiceMetadata, StatusFieldMetadata, StatusItemMetadata, StatusModuleMetadata,
    StatusSchema,
};
use loki_services_macros::loki_status;

#[loki_status]
fn network_status_metadata() -> StatusModuleMetadata {
    let choices = [
        (
            ConnectionStatus::Disconnected,
            "未连接",
            "地址或令牌未配置完整，或服务已关闭。",
        ),
        (
            ConnectionStatus::Connected,
            "连接成功",
            "已连接配置中的中转服务器。",
        ),
        (
            ConnectionStatus::Failed,
            "连接失败",
            "连接失败且没有保留的可用连接。",
        ),
        (
            ConnectionStatus::Connecting,
            "连接中",
            "正在注册或等待连接。",
        ),
    ]
    .into_iter()
    .map(|(value, label, description)| StatusChoiceMetadata {
        value: serde_json::to_value(value).expect("连接状态可以序列化"),
        label: label.to_owned(),
        description: description.to_owned(),
    })
    .collect();
    StatusModuleMetadata {
        id: "network".to_owned(),
        name: "网络".to_owned(),
        description: "本机网络连接状态。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![StatusItemMetadata {
            key: "my".to_owned(),
            name: "本机节点".to_owned(),
            description: "由网络模块发布的本机连接状态。".to_owned(),
            schema: StatusSchema::Object {
                fields: vec![StatusFieldMetadata::new(
                    "status",
                    "连接状态",
                    "当前中转连接状态。",
                    StatusSchema::String { choices },
                )],
            },
        }],
    }
}
