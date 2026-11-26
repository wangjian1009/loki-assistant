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
    status::model::{StatusItemMetadata, StatusModuleMetadata, StatusSchema},
};

const UNREAD_COUNT: &str = "unread_count";
const QUERY_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct IndexedMail {
    message_id: String,
    subject: Option<String>,
}

#[loki_status]
fn mail_status() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: MODULE_ID.into(),
        name: "邮件".into(),
        description: "收件箱的未读邮件状态。".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        items: vec![StatusItemMetadata::new(
            UNREAD_COUNT,
            "未读邮件数",
            "收件箱中未读且未标记删除的邮件数量。",
            StatusSchema::Integer,
        )],
    }
}

impl MailService {
    pub(super) async fn refresh_unread(
        &self,
        seen: &mut Option<HashSet<String>>,
        database: Option<&MuDatabase>,
    ) -> Result<(), String> {
        let messages = unread_mail(database).await?;
        self.status
            .publish(MODULE_ID, UNREAD_COUNT, json!(messages.len()))
            .await
            .map_err(|error| format!("发布未读邮件数失败：{error}"))?;
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
                    body: Some(
                        mail.subject
                            .as_deref()
                            .filter(|value| !value.is_empty())
                            .unwrap_or("无主题")
                            .chars()
                            .take(200)
                            .collect(),
                    ),
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
        ["--nocolor", "--format=json2", "--skip-dups", UNREAD_QUERY],
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
