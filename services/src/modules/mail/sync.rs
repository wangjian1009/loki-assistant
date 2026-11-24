use std::{
    collections::HashSet,
    path::PathBuf,
    process::{Output, Stdio},
    time::Duration,
};

use serde::Deserialize;
use tokio::{io::AsyncReadExt, process::Command, time};

use super::{ENABLED, MODULE_ID, MailService};
use crate::modules::notification::protocol::NotificationRequest;

const INTERVAL: Duration = Duration::from_secs(300);
const SYNC_DEADLINE: Duration = Duration::from_secs(120);
const INDEX_DEADLINE: Duration = Duration::from_secs(60);
const INDEX_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MU_LOCK_BACKOFF: [u64; 4] = [250, 500, 1000, 2000];
pub(super) const CONFIG_CALLBACK: &str = "/mail/config/open";

#[derive(Deserialize)]
pub(super) struct MuDatabase {
    pub(super) program: PathBuf,
    pub(super) home: PathBuf,
}

impl MailService {
    pub(super) async fn sync_loop(&self) {
        let mut changes = self.config.subscribe_module(MODULE_ID);
        let mut seen = None;
        loop {
            let enabled = self
                .config
                .value(MODULE_ID, ENABLED)
                .await
                .is_ok_and(|value| value.value.as_bool() == Some(true));
            if !enabled {
                if changes.recv().await.is_none() {
                    return;
                }
                continue;
            }
            self.sync_cycle(&mut seen).await;
            tokio::select! {
                change = changes.recv() => {
                    if change.is_none() { return; }
                }
                _ = time::sleep(INTERVAL) => {}
            }
        }
    }

    async fn sync_cycle(&self, seen: &mut Option<HashSet<String>>) {
        let Ok(path) = self.config_path().await else {
            self.notify_failure("无法读取 mbsync 配置路径。".into());
            return;
        };
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
        let index_error = match self.update_index().await {
            Ok(database) => self.refresh_unread(seen, database.as_ref()).await.err(),
            Err(error) => Some(error),
        };

        let errors: Vec<_> = [sync_error, index_error].into_iter().flatten().collect();
        if !errors.is_empty() {
            self.notify_failure(errors.join(" "));
        } else {
            log::info!("邮件同步和索引已完成，未读数已刷新");
        }
    }

    async fn update_index(&self) -> Result<Option<MuDatabase>, String> {
        if let Some(database) = self.index_in_emacs().await? {
            return Ok(Some(database));
        }
        let output = run_mu("index", ["--quiet", "--nocolor"], INDEX_DEADLINE, None).await?;
        if output.status.success() {
            return Ok(None);
        }
        // mu4e 可能在首次检查之后启动并取得数据库锁。
        if database_locked(&output)
            && let Some(database) = self.index_in_emacs().await?
        {
            return Ok(Some(database));
        }
        Err(exit_message("mu index", &output))
    }

    async fn index_in_emacs(&self) -> Result<Option<MuDatabase>, String> {
        let request = uuid::Uuid::new_v4();
        time::timeout(INDEX_DEADLINE, async {
            let mut started = false;
            loop {
                let check_only = if started { "t" } else { "nil" };
                let script = format!(
                    r#"(if (and (fboundp 'mu4e-running-p) (mu4e-running-p))
    (progn
      (require 'loki-assistant-mail)
      (loki-assistant-mail-index "{request}" {check_only}))
  'unavailable)"#
                );
                let result = self
                    .emacs
                    .eval_running(&script)
                    .await
                    .map_err(|error| format!("mu4e 索引失败：{error}"))?;
                match result.as_deref() {
                    None | Some("unavailable") if !started => return Ok(None),
                    None | Some("unavailable") => {
                        return Err("mu4e 已退出，无法确认索引结果。".into());
                    }
                    Some("busy") if !started => {}
                    Some("running") => started = true,
                    Some(result) if started => {
                        let database = serde_json::from_str::<String>(result)
                            .and_then(|json| serde_json::from_str::<MuDatabase>(&json))
                            .map_err(|_| "无法读取 mu4e 返回的索引位置。".to_owned())?;
                        return Ok(Some(database));
                    }
                    _ => return Err("mu4e 返回的索引状态无效。".into()),
                }
                time::sleep(INDEX_POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| "等待 mu4e 索引完成超时。".to_owned())?
    }

    fn notify_failure(&self, body: String) {
        log::warn!("邮箱同步失败：{body}");
        self.notification.trigger(NotificationRequest {
            app: Some(MODULE_ID.into()),
            title: "邮箱同步失败".into(),
            body: Some(body),
            callback_url: Some(CONFIG_CALLBACK.into()),
        });
    }
}

pub(super) async fn run_mu<const N: usize>(
    subcommand: &str,
    args: [&str; N],
    deadline: Duration,
    database: Option<&MuDatabase>,
) -> Result<Output, String> {
    let name = format!("mu {subcommand}");
    let mut backoff = MU_LOCK_BACKOFF.into_iter();
    loop {
        let mut command = match database {
            Some(database) => Command::new(&database.program),
            None => Command::new("mu"),
        };
        command.arg(subcommand);
        if let Some(database) = database {
            command.arg("--muhome").arg(&database.home);
        }
        let output = run(command.args(args), &name, deadline).await?;
        if !database_locked(&output) {
            return Ok(output);
        }
        let Some(delay) = backoff.next() else {
            return Ok(output);
        };
        time::sleep(Duration::from_millis(delay)).await;
    }
}

fn database_locked(output: &Output) -> bool {
    output.status.code() == Some(19)
        && String::from_utf8_lossy(&output.stderr).contains("database locked")
}

async fn run(command: &mut Command, name: &str, deadline: Duration) -> Result<Output, String> {
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 {name}（{}）。", error.kind()))?;
    let mut stdout_pipe = child.stdout.take().expect("已配置标准输出管道");
    let mut stderr_pipe = child.stderr.take().expect("已配置标准错误管道");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = time::timeout(deadline, async {
        let (status, _, _) = tokio::try_join!(
            child.wait(),
            stdout_pipe.read_to_end(&mut stdout),
            stderr_pipe.read_to_end(&mut stderr),
        )?;
        Ok::<_, std::io::Error>(Output {
            status,
            stdout,
            stderr,
        })
    })
    .await;
    let message = match result {
        Ok(Ok(output)) => return Ok(output),
        Ok(Err(error)) => format!("读取 {name} 执行结果失败（{}）。", error.kind()),
        Err(_) => format!("{name} 执行超时。"),
    };
    let _ = child.kill().await;
    Err(message)
}

pub(super) fn exit_message(name: &str, output: &Output) -> String {
    if database_locked(output) {
        return format!("{name} 重试后仍无法打开已锁定的数据库。");
    }
    match output.status.code() {
        Some(code) => format!("{name} 执行失败（退出码 {code}）。"),
        None => format!("{name} 已中断。"),
    }
}
