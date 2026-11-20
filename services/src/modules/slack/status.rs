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
        description: "Slack 项目、工作区及连接状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![StatusItemMetadata {
            key: "projects".into(),
            name: "Slack 项目".into(),
            description: "各项目的授权和连接状态。".into(),
            remote_access: false,
            schema: StatusSchema::Array {
                items: Box::new(StatusSchema::Object {
                    fields: vec![
                        Field::new(
                            "id",
                            "项目标识",
                            "配置中的项目标识。",
                            StatusSchema::string(),
                        ),
                        Field::new("name", "项目", "项目显示名称。", StatusSchema::string()),
                        Field::new(
                            "team_id",
                            "工作区 ID",
                            "Slack Team 标识。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "team_name",
                            "工作区",
                            "已授权工作区名称。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "password_id",
                            "密码条目 ID",
                            "应用级令牌在 pass 中的相对路径。",
                            StatusSchema::string(),
                        )
                        .nullable(),
                        Field::new(
                            "app_token_set",
                            "应用令牌已配置",
                            "本机已保存应用令牌。",
                            StatusSchema::Boolean,
                        ),
                        Field::new(
                            "authorization_state",
                            "授权状态",
                            "项目的 Slack OAuth 状态。",
                            StatusSchema::string(),
                        ),
                        Field::new(
                            "socket_state",
                            "连接状态",
                            "项目的 Socket Mode 状态。",
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
        let _ = self.status.publish("slack", "projects", value);
    }
}
