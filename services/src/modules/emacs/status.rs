use crate::modules::status::model::{
    StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema,
};
use loki_services_macros::loki_status;

#[loki_status]
fn emacs_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "emacs".to_owned(),
        name: "Emacs".to_owned(),
        description: "Emacs 主动上报的运行信息。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![
            StatusItemMetadata::new(
                "runtime",
                "运行信息",
                "Emacs 进程与最近上报时间。",
                StatusSchema::Object {
                    fields: vec![
                        Field::new("pid", "进程 ID", "Emacs 进程 ID。", StatusSchema::Integer),
                        Field::new("version", "版本", "Emacs 版本。", StatusSchema::string()),
                        Field::new(
                            "started_at",
                            "启动时间",
                            "Emacs 启动时间，Unix 秒。",
                            StatusSchema::Integer,
                        ),
                        Field::new(
                            "reported_at",
                            "上报时间",
                            "最近一次主动上报时间，Unix 秒。",
                            StatusSchema::Integer,
                        ),
                    ],
                },
            )
            .local_only(),
        ],
    }
}
