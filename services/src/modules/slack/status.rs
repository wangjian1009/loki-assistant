use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use loki_services_macros::loki_status;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::modules::status::{
    StatusService,
    model::{StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema},
};

use super::{
    notification,
    store::{Account, Cache},
};

#[loki_status]
fn slack_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "slack".into(),
        name: "Slack".into(),
        description: "Slack 授权、连接和新消息会话状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            StatusItemMetadata {
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
                                "最近一次授权或连接错误。",
                                StatusSchema::string(),
                            )
                            .nullable(),
                        ],
                    }),
                },
            },
            StatusItemMetadata {
                key: "new_conversations".into(),
                name: "Slack 新消息会话".into(),
                description: "含有符合通知规则的未读消息的会话，按最新匹配消息排序。".into(),
                remote_access: false,
                schema: StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new(
                                "id",
                                "会话 ID",
                                "包含工作区 ID 的会话标识。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "title",
                                "会话名称",
                                "本地会话显示名称。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "team_name",
                                "工作区",
                                "授权工作区名称。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "session_id",
                                "授权会话 ID",
                                "用于验证打开请求对应的本机授权。",
                                StatusSchema::string(),
                            ),
                            Field::new(
                                "callback",
                                "打开路径",
                                "定位最新匹配消息的本机 POST 路径。",
                                StatusSchema::string(),
                            ),
                        ],
                    }),
                },
            },
        ],
    }
}

#[derive(PartialEq, Serialize)]
struct NewConversation {
    id: String,
    title: String,
    team_name: String,
    session_id: Uuid,
    callback: String,
    #[serde(skip)]
    order: (u64, u32),
}

struct ProjectSnapshot {
    generation: Uuid,
    connection: Value,
    new_conversations: Vec<NewConversation>,
}

pub(super) struct ProjectsStatus {
    status: Arc<StatusService>,
    entries: Mutex<BTreeMap<String, ProjectSnapshot>>,
}

impl ProjectsStatus {
    pub fn new(status: Arc<StatusService>) -> Self {
        let snapshot = Self {
            status,
            entries: Mutex::default(),
        };
        let entries = snapshot.entries.lock().unwrap();
        snapshot.publish_connections(&entries);
        snapshot.publish_new_conversations(&entries);
        drop(entries);
        snapshot
    }

    pub fn register(&self, id: &str, generation: Uuid, value: Value) {
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            id.to_owned(),
            ProjectSnapshot {
                generation,
                connection: value,
                new_conversations: Vec::new(),
            },
        );
        self.publish_connections(&entries);
    }

    pub fn update(&self, id: &str, generation: Uuid, value: Value) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(project) = entries.get_mut(id)
            && project.generation == generation
            && project.connection != value
        {
            project.connection = value;
            self.publish_connections(&entries);
        }
    }

    pub fn update_new_conversations(
        &self,
        id: &str,
        generation: Uuid,
        authorized: Option<(&Cache, &Account)>,
    ) {
        let new_conversations = authorized.map_or_else(Vec::new, |(cache, account)| {
            cache
                .conversations
                .values()
                .filter_map(|room| {
                    let (&order, target) =
                        room.notification_targets
                            .iter()
                            .rev()
                            .find(|(order, target)| {
                                room.read_positions.after_cursor(
                                    **order,
                                    target.thread_ts.as_deref(),
                                    account,
                                )
                            })?;
                    let id = format!("{id}.{}", room.id);
                    Some(NewConversation {
                        callback: notification::callback(
                            &id,
                            &target.ts,
                            target.thread_ts.as_deref(),
                        ),
                        id,
                        title: room.title.clone(),
                        team_name: account.identity.team_name.clone(),
                        session_id: account.identity.session_id,
                        order,
                    })
                })
                .collect()
        });
        let mut entries = self.entries.lock().unwrap();
        if let Some(project) = entries.get_mut(id)
            && project.generation == generation
            && project.new_conversations != new_conversations
        {
            project.new_conversations = new_conversations;
            self.publish_new_conversations(&entries);
        }
    }

    pub fn remove(&self, id: &str) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(project) = entries.remove(id) {
            self.publish_connections(&entries);
            if !project.new_conversations.is_empty() {
                self.publish_new_conversations(&entries);
            }
        }
    }

    fn publish_connections(&self, entries: &BTreeMap<String, ProjectSnapshot>) {
        let value = json!(
            entries
                .values()
                .map(|project| &project.connection)
                .collect::<Vec<_>>()
        );
        let _ = self.status.publish("slack", "connections", value);
    }

    fn publish_new_conversations(&self, entries: &BTreeMap<String, ProjectSnapshot>) {
        let mut conversations: Vec<_> = entries
            .values()
            .flat_map(|project| &project.new_conversations)
            .collect();
        conversations.sort_unstable_by(|left, right| {
            right
                .order
                .cmp(&left.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        let _ = self
            .status
            .publish("slack", "new_conversations", json!(conversations));
    }
}
