use std::{collections::HashSet, time::Duration};

use loki_services_macros::loki_status;
use serde::Deserialize;
use serde_json::json;

use super::{
    MODULE_ID, MailService, UNREAD_QUERY,
    sync::{self, MuDatabase},
};
use crate::modules::{
    notification::protocol::NotificationRequest,
    status::model::{
        StatusFieldMetadata as Field, StatusItemMetadata, StatusModuleMetadata, StatusSchema,
    },
};

const UNREAD_COUNT: &str = "unread_count";
const UNREAD_SUBJECTS: &str = "unread_subjects";
pub(super) const SYNC_RUNS: &str = "sync_runs";
const QUERY_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct IndexedMail {
    message_id: String,
    subject: Option<String>,
}

impl IndexedMail {
    fn display_subject(&self) -> String {
        self.subject
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("无主题")
            .chars()
            .take(200)
            .collect()
    }
}

#[loki_status]
fn mail_status() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: MODULE_ID.into(),
        name: "邮件".into(),
        description: "收件箱的未读邮件状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![
            StatusItemMetadata::new(
                UNREAD_COUNT,
                "未读邮件数",
                "收件箱中未读且未标记删除的邮件数量。",
                StatusSchema::Integer,
            ),
            StatusItemMetadata::new(
                UNREAD_SUBJECTS,
                "未读邮件主题",
                "收件箱中最近 20 封未读邮件的主题，按时间倒序排列。",
                StatusSchema::Array {
                    items: Box::new(StatusSchema::string()),
                },
            ),
            StatusItemMetadata::new(
                SYNC_RUNS,
                "邮件同步进度",
                "当前和最近的邮件同步任务及其阶段。",
                StatusSchema::Array {
                    items: Box::new(StatusSchema::Object {
                        fields: vec![
                            Field::new("id", "任务 ID", "同步任务标识。", StatusSchema::string()),
                            Field::new("phase", "阶段", "邮件同步阶段。", StatusSchema::string()),
                        ],
                    }),
                },
            ),
        ],
    }
}

impl MailService {
    pub(super) async fn refresh_unread(
        &self,
        seen: &mut Option<HashSet<String>>,
        database: Option<&MuDatabase>,
    ) -> Result<(), String> {
        let messages = unread_mail(database).await?;
        let subjects: Vec<_> = messages
            .iter()
            .take(20)
            .map(IndexedMail::display_subject)
            .collect();
        self.status
            .publish_many(
                [(
                    MODULE_ID.into(),
                    [
                        (UNREAD_COUNT.into(), json!(messages.len())),
                        (UNREAD_SUBJECTS.into(), json!(subjects)),
                    ]
                    .into(),
                )]
                .into(),
            )
            .await
            .map_err(|error| format!("发布未读邮件失败：{error}"))?;
        let Some(seen) = seen else {
            *seen = Some(messages.into_iter().map(|mail| mail.message_id).collect());
            return Ok(());
        };
        for mail in messages {
            if seen.insert(mail.message_id.clone()) {
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("message_id", &mail.message_id)
                    .finish();
                self.notification.trigger(NotificationRequest {
                    app: Some(MODULE_ID.into()),
                    title: "新邮件".into(),
                    body: Some(mail.display_subject()),
                    callback_url: Some(format!("/emacs/mail/open?{query}")),
                });
            }
        }
        Ok(())
    }
}

async fn unread_mail(database: Option<&MuDatabase>) -> Result<Vec<IndexedMail>, String> {
    let output = sync::run_mu(
        "find",
        [
            "--nocolor",
            "--format=json2",
            "--skip-dups",
            "--sortfield=date",
            "--reverse",
            UNREAD_QUERY,
        ],
        QUERY_DEADLINE,
        database,
        None,
    )
    .await?;
    if output.status.code() == Some(2)
        && String::from_utf8_lossy(&output.stderr).contains("no matches for search expression")
    {
        return Ok(Vec::new());
    }
    if !output.status.success() {
        return Err(sync::exit_message("mu find", &output));
    }
    serde_json::from_slice(&output.stdout).map_err(|_| "无法读取 mu 的未读邮件查询结果。".into())
}
