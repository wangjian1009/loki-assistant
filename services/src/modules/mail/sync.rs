use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

use serde::Deserialize;
use tokio::{process::Command, time};

use super::{ENABLED, MODULE_ID, MailService};
use crate::modules::{
    alert::{AlertInput, AlertLevel},
    notification::protocol::NotificationRequest,
};

const INTERVAL: Duration = Duration::from_secs(300);
const SYNC_DEADLINE: Duration = Duration::from_secs(120);
const INDEX_DEADLINE: Duration = Duration::from_secs(60);
const ALERT_ID: &str = "mail-sync";
const APP_ID: &str = "mail";
pub(super) const CONFIG_CALLBACK: &str = "/mail/config/open";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct IndexedMail {
    message_id: String,
    subject: Option<String>,
}

impl MailService {
    pub(super) async fn sync_loop(&self) {
        let mut changes = self.config.subscribe_module(MODULE_ID);
        let mut baseline = None;
        let mut previous_path = None;

        loop {
            let enabled = self
                .config
                .value(MODULE_ID, ENABLED)
                .await
                .is_ok_and(|value| value.value.as_bool() == Some(true));
            if !enabled {
                self.clear_alert().await;
                if changes.recv().await.is_none() {
                    return;
                }
                continue;
            }
            tokio::select! {
                biased;
                change = changes.recv() => {
                    if change.is_none() { return; }
                    continue;
                }
                _ = self.sync_cycle(&mut previous_path, &mut baseline) => {}
            }
            tokio::select! {
                change = changes.recv() => {
                    if change.is_none() { return; }
                }
                _ = time::sleep(INTERVAL) => {}
            }
        }
    }

    async fn sync_cycle(
        &self,
        previous_path: &mut Option<PathBuf>,
        baseline: &mut Option<HashSet<String>>,
    ) {
        let Ok(path) = self.config_path().await else {
            self.report_alert("无法读取 mbsync 配置路径。").await;
            return;
        };
        if previous_path.as_ref() != Some(&path) {
            *baseline = None;
            *previous_path = Some(path.clone());
        }
        if baseline.is_none() {
            match inbox().await {
                Ok(messages) => *baseline = Some(messages.into_keys().collect()),
                Err(error) => {
                    self.report_alert(&error).await;
                    return;
                }
            }
        }

        let sync_error = match run(
            Command::new("mbsync")
                .args(["-q", "-c"])
                .arg(&path)
                .arg("loki-auto"),
            "mbsync",
            SYNC_DEADLINE,
        )
        .await
        {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(exit_message("mbsync", &output)),
            Err(error) => Some(error),
        };

        // 某个邮箱同步失败时，其他邮箱可能已经写入 Maildir。
        let result = self.index().await;
        let index_error = match result {
            Ok(()) => match inbox().await {
                Ok(messages) => {
                    let seen = baseline.as_ref().expect("已读取同步前的索引");
                    for (message_id, subject) in &messages {
                        if !seen.contains(message_id) {
                            let query = url::form_urlencoded::Serializer::new(String::new())
                                .append_pair("message_id", message_id)
                                .finish();
                            self.notification.trigger(NotificationRequest {
                                app: Some(APP_ID.into()),
                                title: "新邮件".into(),
                                body: Some(
                                    subject
                                        .as_deref()
                                        .filter(|value| !value.is_empty())
                                        .unwrap_or("无主题")
                                        .chars()
                                        .take(200)
                                        .collect(),
                                ),
                                callback_url: Some(format!("/mail/open?{query}")),
                            });
                        }
                    }
                    *baseline = Some(messages.into_keys().collect());
                    None
                }
                Err(error) => Some(error),
            },
            Err(error) => Some(error),
        };

        let errors: Vec<_> = [sync_error, index_error].into_iter().flatten().collect();
        if errors.is_empty() {
            self.clear_alert().await;
        } else {
            self.report_alert(&errors.join(" ")).await;
        }
    }

    async fn index(&self) -> Result<(), String> {
        let output = run(
            Command::new("mu").args(["index", "--quiet", "--nocolor"]),
            "mu index",
            INDEX_DEADLINE,
        )
        .await?;
        if output.status.success() {
            return Ok(());
        }
        if output.status.code() != Some(19)
            || !String::from_utf8_lossy(&output.stderr).contains("database locked")
        {
            return Err(exit_message("mu index", &output));
        }

        // 活跃的 mu4e 持有数据库写锁，使用同一个 mu server 完成索引。
        time::timeout(INDEX_DEADLINE, async {
            let home = mu_home();
            self.wait_for_emacs_index(&home, "start").await?;
            self.wait_for_emacs_index(&home, "wait").await
        })
        .await
        .map_err(|_| "等待 mu4e 索引超时。".to_owned())?
    }

    async fn wait_for_emacs_index(&self, home: &Path, phase: &str) -> Result<(), String> {
        loop {
            let state = self
                .emacs
                .mail_index(home, phase)
                .await
                .map_err(|_| "mu 数据库已锁定，无法通过目标 Emacs 更新索引。".to_owned())?;
            match state.as_str() {
                "done" => return Ok(()),
                "busy" => time::sleep(Duration::from_millis(500)).await,
                _ => return Err("mu 数据库已锁定，目标 Emacs 未运行对应的 mu4e 索引。".into()),
            }
        }
    }

    async fn report_alert(&self, body: &str) {
        if let Err(error) = self
            .alert
            .publish(
                ALERT_ID,
                AlertInput {
                    app: Some(APP_ID.into()),
                    title: "邮箱同步失败".into(),
                    body: Some(body.into()),
                    callback_url: Some(CONFIG_CALLBACK.into()),
                    level: AlertLevel::Warning,
                },
            )
            .await
        {
            eprintln!("发布邮件同步告警失败：{error}");
        }
    }

    async fn clear_alert(&self) {
        if let Err(error) = self.alert.clear(ALERT_ID).await {
            eprintln!("清理邮件同步告警失败：{error}");
        }
    }
}

fn mu_home() -> PathBuf {
    std::env::var_os("MUHOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .filter(|path| !path.is_empty())
                .map(|path| PathBuf::from(path).join("mu"))
        })
        .unwrap_or_else(|| std::env::home_dir().unwrap_or_default().join(".cache/mu"))
}

async fn inbox() -> Result<HashMap<String, Option<String>>, String> {
    let output = run(
        Command::new("mu").args(["find", "--nocolor", "--format=json2", r"maildir:/\/inbox$/"]),
        "mu find",
        INDEX_DEADLINE,
    )
    .await?;
    if output.status.code() == Some(2)
        && String::from_utf8_lossy(&output.stderr).contains("no matches for search expression")
    {
        return Ok(HashMap::new());
    }
    if !output.status.success() {
        return Err(exit_message("mu find", &output));
    }
    let messages: Vec<IndexedMail> = serde_json::from_slice(&output.stdout)
        .map_err(|_| "无法读取 mu 的收件箱查询结果。".to_owned())?;
    Ok(messages
        .into_iter()
        .map(|mail| (mail.message_id, mail.subject))
        .collect())
}

async fn run(command: &mut Command, name: &str, deadline: Duration) -> Result<Output, String> {
    time::timeout(
        deadline,
        command.kill_on_drop(true).stdin(Stdio::null()).output(),
    )
    .await
    .map_err(|_| format!("{name} 执行超时。"))?
    .map_err(|error| format!("无法启动 {name}（{}）。", error.kind()))
}

fn exit_message(name: &str, output: &Output) -> String {
    match output.status.code() {
        Some(code) => format!("{name} 执行失败（退出码 {code}）。"),
        None => format!("{name} 已中断。"),
    }
}
