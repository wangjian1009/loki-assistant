use crate::modules::status::model::{
    StatusChoiceMetadata, StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata,
    StatusSchema,
};
use loki_services_macros::loki_status;

#[loki_status]
fn emacs_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "emacs".to_owned(),
        name: "Emacs".to_owned(),
        description: "Emacs 主动上报的运行信息及 Agent Shell 会话状态。".to_owned(),
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
            StatusItemMetadata::new(
                "agent_shell",
                "Agent Shell 执行状态",
                "Emacs 当前打开的 Agent Shell 会话、标题及执行阶段。",
                StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new(
                                "id",
                                "会话标识",
                                "由 Emacs 分配的临时会话标识。",
                                StatusSchema::string(),
                            ),
                            Field {
                                required: false,
                                ..Field::new(
                                    "title",
                                    "会话标题",
                                    "Emacs 中可用的 Agent Shell 会话标题。",
                                    StatusSchema::string(),
                                )
                            },
                            Field::new(
                                "phase",
                                "执行阶段",
                                "当前会话的执行阶段。",
                                choices(&[
                                    ("idle", "空闲", "会话可接收新提示。"),
                                    ("starting", "启动中", "会话正在初始化。"),
                                    ("running", "执行中", "Agent 正在处理当前轮次。"),
                                    ("waiting_input", "等待输入", "当前轮次等待授权或其他输入。"),
                                    ("completed", "已完成", "当前轮次已正常结束。"),
                                    ("cancelled", "已取消", "当前轮次已取消。"),
                                    ("failed", "失败", "当前轮次执行出错。"),
                                    ("disconnected", "已断开", "会话与 Agent 的连接已关闭。"),
                                ]),
                            ),
                            Field {
                                required: false,
                                ..Field::new(
                                    "stop_reason",
                                    "停止原因",
                                    "Agent 返回的标准化轮次停止原因。",
                                    choices(&[
                                        ("end_turn", "轮次结束", "Agent 正常结束轮次。"),
                                        ("max_tokens", "达到 token 上限", "轮次达到 token 上限。"),
                                        (
                                            "max_turn_requests",
                                            "达到请求次数上限",
                                            "轮次达到请求次数上限。",
                                        ),
                                        ("refusal", "Agent 拒绝", "Agent 拒绝继续执行。"),
                                        ("cancelled", "已取消", "轮次已取消。"),
                                        ("other", "其他", "Agent 提供了其他停止原因。"),
                                    ]),
                                )
                            },
                        ],
                    }),
                },
            ),
        ],
    }
}

fn choices(values: &[(&str, &str, &str)]) -> StatusSchema {
    StatusSchema::String {
        choices: values
            .iter()
            .map(|(value, label, description)| StatusChoiceMetadata {
                value: (*value).into(),
                label: (*label).to_owned(),
                description: (*description).to_owned(),
            })
            .collect(),
    }
}
