use crate::modules::status::model::{
    StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema,
};
use loki_services_macros::loki_status;

#[loki_status]
fn emacs_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "emacs".to_owned(),
        name: "Emacs".to_owned(),
        description: "Emacs 主动上报的关注 buffer 与运行信息。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![
            StatusItemMetadata {
                key: "buffers".to_owned(),
                name: "关注的 buffer".to_owned(),
                description: "关注的 mode 及其派生 mode 的 buffer 列表；没有关注项时为空数组。"
                    .to_owned(),
                schema: StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new(
                                "name",
                                "名称",
                                "Emacs buffer 名称。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "mode",
                                "主要模式",
                                "major mode 的符号名。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "directory",
                                "工作目录",
                                "buffer 的 default-directory。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "file",
                                "文件",
                                "关联文件路径，非文件 buffer 为 null。",
                                StatusSchema::string(),
                            )
                            .nullable(),
                            Field::new(
                                "modified",
                                "是否修改",
                                "buffer 是否存在未保存修改。",
                                StatusSchema::Boolean,
                            ),
                        ],
                    }),
                },
            },
            StatusItemMetadata {
                key: "runtime".to_owned(),
                name: "运行信息".to_owned(),
                description: "Emacs 进程与最近上报时间。".to_owned(),
                schema: StatusSchema::Object {
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
            },
        ],
    }
}
