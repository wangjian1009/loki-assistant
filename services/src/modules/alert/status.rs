use loki_services_macros::loki_status;
use serde_json::json;

use super::{ACTIVE_KEY, AlertLevel, MODULE_ID};
use crate::modules::status::model::{
    StatusChoiceMetadata, StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata,
    StatusSchema,
};

#[loki_status]
fn alert_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: MODULE_ID.into(),
        name: "告警".into(),
        description: "当前有效的本机告警。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            StatusItemMetadata::new(
                ACTIVE_KEY,
                "活跃告警",
                "当前有效的告警，按 ID 排序。",
                StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new("id", "ID", "调用方提供的标识。", StatusSchema::string()),
                            Field::new(
                                "app",
                                "应用",
                                "通知使用的应用标识。",
                                StatusSchema::string(),
                            )
                            .nullable(),
                            Field::new("title", "标题", "告警标题。", StatusSchema::string()),
                            Field::new("body", "正文", "告警正文。", StatusSchema::string())
                                .nullable(),
                            Field {
                                required: false,
                                ..Field::new(
                                    "callback_url",
                                    "回调路径",
                                    "点击通知时调用的本机服务路径。",
                                    StatusSchema::string(),
                                )
                            },
                            Field::new("level", "级别", "告警级别。", level_schema()),
                            Field::new(
                                "created_at",
                                "创建时间",
                                "首次发布时间，以 Unix 秒表示。",
                                StatusSchema::Integer,
                            ),
                            Field::new(
                                "updated_at",
                                "更新时间",
                                "最近一次内容变更时间，以 Unix 秒表示。",
                                StatusSchema::Integer,
                            ),
                        ],
                    }),
                },
            )
            .local_only(),
        ],
    }
}

fn level_schema() -> StatusSchema {
    StatusSchema::String {
        choices: [
            (AlertLevel::Info, "信息", "用于提示业务状态。"),
            (AlertLevel::Warning, "警告", "用于提示需要关注的情况。"),
            (AlertLevel::Error, "错误", "用于报告业务错误。"),
        ]
        .into_iter()
        .map(|(level, label, description)| StatusChoiceMetadata {
            value: json!(level),
            label: label.into(),
            description: description.into(),
        })
        .collect(),
    }
}
