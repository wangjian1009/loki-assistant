use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use loki_services_macros::loki_status;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::modules::status::{
    StatusService,
    model::{StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema},
};

#[loki_status]
fn slack_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "slack".into(),
        name: "Slack".into(),
        description: "Slack 授权和连接状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![StatusItemMetadata {
            key: "connections".into(),
            name: "Slack 连接".into(),
            description: "各工作区的授权和连接状态。".into(),
            remote_access: false,
            schema: StatusSchema::Array {
                items: Box::new(StatusSchema::Object {
                    fields: vec![
                        Field::new(
                            "team_id",
                            "工作区 ID",
                            "Slack Team 标识。",
                            StatusSchema::string(),
                        ),
                        Field::new("name", "名称", "本地显示名称。", StatusSchema::string()),
                        Field::new(
                            "team_name",
                            "工作区",
                            "已授权工作区名称。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "session_id",
                            "会话 ID",
                            "当前本机授权会话的标识。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "user_id",
                            "用户 ID",
                            "由工作区 ID 和授权用户的 Slack ID 组成。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "authorization_state",
                            "授权状态",
                            "当前工作区的用户凭据状态。",
                            StatusSchema::string(),
                        ),
                        Field::new(
                            "socket_state",
                            "连接状态",
                            "当前工作区的 Slack WebSocket 连接状态。",
                            StatusSchema::string(),
                        ),
                        Field::new(
                            "issue_code",
                            "问题代码",
                            "最近一次连接或存储错误。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                    ],
                }),
            },
        }],
    }
}

pub(super) struct ProjectsStatus {
    status: Arc<StatusService>,
    entries: Mutex<BTreeMap<String, (Uuid, Value)>>,
}

impl ProjectsStatus {
    pub fn new(status: Arc<StatusService>) -> Self {
        let snapshot = Self {
            status,
            entries: Mutex::default(),
        };
        snapshot.publish(&snapshot.entries.lock().unwrap());
        snapshot
    }

    pub fn register(&self, id: &str, generation: Uuid, value: Value) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(id.to_owned(), (generation, value));
        self.publish(&entries);
    }

    pub fn update(&self, id: &str, generation: Uuid, value: Value) {
        let mut entries = self.entries.lock().unwrap();
        if let Some((current, data)) = entries.get_mut(id)
            && *current == generation
            && *data != value
        {
            *data = value;
            self.publish(&entries);
        }
    }

    pub fn remove(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(id);
        self.publish(&entries);
    }

    fn publish(&self, entries: &BTreeMap<String, (Uuid, Value)>) {
        let value = json!(entries.values().map(|(_, data)| data).collect::<Vec<_>>());
        let _ = self.status.publish("slack", "connections", value);
    }
}
