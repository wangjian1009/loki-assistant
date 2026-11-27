use crate::process::Process;

use std::{collections::BTreeMap, sync::Arc};

use loki_services_macros::loki_status;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::modules::status::{
    StatusService,
    model::{StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema},
};

use super::{
    mailbox::UserDirectory,
    notification,
    protocol::timestamp,
    store::{Account, Cache},
};

#[loki_status]
fn slack_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "slack".into(),
        name: "Slack".into(),
        description: "Slack 授权、连接和未读会话状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            StatusItemMetadata::new(
                "connections",
                "Slack 连接",
                "各工作区的授权和连接状态。",
                StatusSchema::Array {
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
            )
            .local_only(),
            StatusItemMetadata::new(
                "new_conversations",
                "Slack 未读会话",
                "包含未读消息的会话，按最新未读消息排序。",
                StatusSchema::Array {
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
                                "unread_count",
                                "未读数",
                                "本机已同步的未读消息数。",
                                StatusSchema::Integer,
                            ),
                            Field::new(
                                "callback",
                                "打开路径",
                                "定位最新未读消息的本机 POST 路径。",
                                StatusSchema::string(),
                            ),
                        ],
                    }),
                },
            )
            .local_only(),
            StatusItemMetadata::new(
                "unread_previews",
                "Slack 未读摘要",
                "最近 20 个未读会话的名称、工作区、未读数及最新未读消息摘要。",
                StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new("id", "会话 ID", "会话标识。", StatusSchema::string()),
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
                                "unread_count",
                                "未读数",
                                "本机已同步的未读消息数。",
                                StatusSchema::Integer,
                            ),
                            Field::new(
                                "preview",
                                "摘要",
                                "最新未读消息的纯文本摘要。",
                                StatusSchema::string(),
                            ),
                        ],
                    }),
                },
            ),
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
    unread_count: usize,
    #[serde(skip)]
    preview: String,
    #[serde(skip)]
    order: (u64, u32),
}

struct ProjectSnapshot {
    generation: Uuid,
    connection: Value,
    new_conversations: Vec<NewConversation>,
}

pub(super) struct ProjectsStatus {
    entries: Process<Entries>,
}

struct Entries {
    status: Arc<StatusService>,
    projects: BTreeMap<String, ProjectSnapshot>,
}

impl ProjectsStatus {
    pub fn new(status: Arc<StatusService>) -> Self {
        status.post("slack", "connections", json!([]));
        status.post("slack", "new_conversations", json!([]));
        status.post("slack", "unread_previews", json!([]));
        Self {
            entries: Process::start(Entries {
                status,
                projects: BTreeMap::new(),
            }),
        }
    }

    pub async fn register(&self, id: &str, generation: Uuid, value: Value) {
        let id = id.to_owned();
        self.entries
            .call(move |entries| {
                entries.projects.insert(
                    id,
                    ProjectSnapshot {
                        generation,
                        connection: value,
                        new_conversations: Vec::new(),
                    },
                );
                entries.publish_connections();
            })
            .await;
    }

    pub async fn update(&self, id: &str, generation: Uuid, value: Value) {
        let id = id.to_owned();
        self.entries
            .call(move |entries| {
                if let Some(project) = entries.projects.get_mut(&id)
                    && project.generation == generation
                    && project.connection != value
                {
                    project.connection = value;
                    entries.publish_connections();
                }
            })
            .await;
    }

    pub fn update_new_conversations(
        &self,
        id: &str,
        generation: Uuid,
        authorized: Option<(&Cache, &Account, Option<&UserDirectory>)>,
    ) {
        let new_conversations = authorized.map_or_else(Vec::new, |(cache, account, users)| {
            cache
                .conversations
                .values()
                .filter_map(|room| {
                    let mut unread = room.unread_messages(account);
                    let first = unread.next()?;
                    let (unread_count, target) =
                        unread.fold((1, first), |(count, latest), message| {
                            let latest = if timestamp(&message.ts) > timestamp(&latest.ts) {
                                message
                            } else {
                                latest
                            };
                            (count + 1, latest)
                        });
                    let order = timestamp(&target.ts)?;
                    let id = format!("{id}.{}", room.id);
                    Some(NewConversation {
                        callback: notification::callback(
                            &id,
                            &target.ts,
                            target.thread_ts.as_deref(),
                        ),
                        id,
                        title: room.display_title(account, users),
                        team_name: account.identity.team_name.clone(),
                        session_id: account.identity.session_id,
                        unread_count,
                        preview: notification::body_with_nicknames(target, users, Some(account))
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(240)
                            .collect(),
                        order,
                    })
                })
                .collect()
        });
        let id = id.to_owned();
        self.entries.post(move |entries| {
            if let Some(project) = entries.projects.get_mut(&id)
                && project.generation == generation
                && project.new_conversations != new_conversations
            {
                project.new_conversations = new_conversations;
                entries.publish_new_conversations();
            }
        });
    }

    pub async fn remove(&self, id: &str) {
        let id = id.to_owned();
        self.entries
            .call(move |entries| {
                if let Some(project) = entries.projects.remove(&id) {
                    entries.publish_connections();
                    if !project.new_conversations.is_empty() {
                        entries.publish_new_conversations();
                    }
                }
            })
            .await;
    }

    pub async fn flush(&self) {
        self.entries.call(|_| ()).await;
    }
}

impl Entries {
    fn publish_connections(&self) {
        let value = json!(
            self.projects
                .values()
                .map(|project| &project.connection)
                .collect::<Vec<_>>()
        );
        self.status.post("slack", "connections", value);
    }

    fn publish_new_conversations(&self) {
        let mut conversations: Vec<_> = self
            .projects
            .values()
            .flat_map(|project| &project.new_conversations)
            .collect();
        conversations.sort_unstable_by(|left, right| {
            right
                .order
                .cmp(&left.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.status
            .post("slack", "new_conversations", json!(conversations));
        let previews: Vec<_> = conversations
            .iter()
            .take(20)
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "title": entry.title.chars().take(128).collect::<String>(),
                    "team_name": entry.team_name.chars().take(128).collect::<String>(),
                    "unread_count": entry.unread_count,
                    "preview": entry.preview,
                })
            })
            .collect();
        self.status
            .post("slack", "unread_previews", json!(previews));
    }
}
