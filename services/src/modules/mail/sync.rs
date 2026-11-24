use std::{
    path::PathBuf,
    process::{Output, Stdio},
    time::Duration,
};

use futures_util::FutureExt;
use serde::Deserialize;
use tokio::{io::AsyncReadExt, process::Command, time};

use super::{ENABLED, MODULE_ID, MailService, diagnostic};
use crate::modules::alert::{AlertInput, AlertLevel};

const INTERVAL: Duration = Duration::from_secs(300);
const SYNC_DEADLINE: Duration = Duration::from_secs(120);
const INDEX_DEADLINE: Duration = Duration::from_secs(60);
const INDEX_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REFRESH_RETRY_INTERVAL: Duration = Duration::from_secs(5);
const MU_LOCK_BACKOFF: [u64; 4] = [250, 500, 1000, 2000];
const SYNC_ALERT: &str = "mail.sync";
const CONFIG_CALLBACK: &str = "/mail/config/open";

#[derive(Deserialize)]
pub(super) struct MuDatabase {
    pub(super) program: PathBuf,
    pub(super) home: PathBuf,
}

impl MuDatabase {
    pub(super) fn from_emacs(result: &str) -> Result<Self, String> {
        serde_json::from_str::<String>(result)
            .and_then(|json| serde_json::from_str(&json))
            .map_err(|_| "无法读取 mu4e 返回的索引位置。".into())
    }
}

impl MailService {
    pub(super) async fn sync_loop(&self) {
        let mut changes = self.config.subscribe_module(MODULE_ID);
        let mut seen = None;
        let mut enabled = self.sync_enabled().await;
        let mut retry_pending = false;
        let next_sync = time::sleep(Duration::ZERO);
        let next_retry = time::sleep(Duration::ZERO);
        tokio::pin!(next_sync, next_retry);
        if !enabled {
            self.clear_sync_alert().await;
        }

        loop {
            let sync_due = tokio::select! {
                biased;
                change = changes.recv() => {
                    if change.is_none() { return; }
                    enabled = self.sync_enabled().await;
                    if !enabled {
                        self.clear_sync_alert().await;
                    }
                    next_sync.as_mut().reset(time::Instant::now());
                    continue;
                }
                _ = &mut next_sync, if enabled => true,
                _ = &mut next_retry, if retry_pending => false,
                _ = self.refresh_requested.notified(), if !retry_pending => false,
            };

            let (sync_error, database) = if sync_due {
                self.sync_and_index().await
            } else {
                (None, self.emacs_database_for_refresh().await.map(Some))
            };
            let mut requested = !sync_due || retry_pending;
            let refresh_error = match database {
                Ok(database) => {
                    // 本次查询覆盖此前到达的请求。查询开始后到达的请求留给下一轮。
                    requested |= self.refresh_requested.notified().now_or_never().is_some();
                    self.refresh_unread(&mut seen, database.as_ref())
                        .await
                        .err()
                }
                Err(error) => Some(error),
            };
            if requested && refresh_error.is_some() {
                if !sync_due
                    && !retry_pending
                    && let Some(error) = &refresh_error
                {
                    log::warn!("邮件状态刷新已推迟：{error}");
                }
                next_retry
                    .as_mut()
                    .reset(time::Instant::now() + REFRESH_RETRY_INTERVAL);
                retry_pending = true;
            } else {
                retry_pending = false;
            }

            if sync_due {
                let errors: Vec<_> = [sync_error, refresh_error].into_iter().flatten().collect();
                if errors.is_empty() {
                    self.clear_sync_alert().await;
                    log::info!("邮件同步和索引已完成，未读数已刷新");
                } else {
                    self.report_failure(errors.join("\n")).await;
                }
                next_sync.as_mut().reset(time::Instant::now() + INTERVAL);
            }
        }
    }

    async fn sync_enabled(&self) -> bool {
        self.config
            .value(MODULE_ID, ENABLED)
            .await
            .is_ok_and(|value| value.value.as_bool() == Some(true))
    }

    async fn sync_and_index(&self) -> (Option<String>, Result<Option<MuDatabase>, String>) {
        let path = match self.config_path().await {
            Ok(path) => path,
            Err(error) => return (None, Err(format!("无法读取 mbsync 配置路径。{error}"))),
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
        (sync_error, self.update_index().await)
    }

    async fn emacs_database_for_refresh(&self) -> Result<MuDatabase, String> {
        let result = self
            .emacs
            .eval_running(
                r#"(cond
  ((not (and (fboundp 'mu4e-running-p) (mu4e-running-p))) 'unavailable)
  ((bound-and-true-p mu4e--server-indexing) 'busy)
  (t (require 'loki-assistant-mail)
     (loki-assistant-mail-database)))"#,
            )
            .await
            .map_err(|error| format!("读取 mu4e 索引位置失败：{error}"))?;
        match result.as_deref() {
            None | Some("unavailable") => Err("mu4e 尚未运行。".into()),
            Some("busy") => Err("mu4e 正在更新索引。".into()),
            Some(result) => MuDatabase::from_emacs(result),
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
                    Some(result) if started => return MuDatabase::from_emacs(result).map(Some),
                    _ => return Err("mu4e 返回的索引状态无效。".into()),
                }
                time::sleep(INDEX_POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| "等待 mu4e 索引完成超时。".to_owned())?
    }

    async fn report_failure(&self, body: String) {
        log::warn!("邮箱同步失败：{}", body.replace('\n', " "));
        if let Err(error) = self
            .alert
            .publish(
                SYNC_ALERT,
                AlertInput {
                    app: Some(MODULE_ID.into()),
                    title: "邮箱同步失败".into(),
                    body: Some(body),
                    callback_url: Some(CONFIG_CALLBACK.into()),
                    level: AlertLevel::Error,
                },
            )
            .await
        {
            log::warn!("发布邮件同步告警失败：{error}");
        }
    }

    async fn clear_sync_alert(&self) {
        if let Err(error) = self.alert.clear(SYNC_ALERT).await {
            log::warn!("清理邮件同步告警失败：{error}");
        }
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
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                format!("无法启动 {name}。请检查可执行文件及 services 的 PATH。")
            }
            std::io::ErrorKind::PermissionDenied => {
                format!("无法启动 {name}。可执行文件的权限不足。")
            }
            _ => format!("无法启动 {name}（{}）。", error.kind()),
        })?;
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
        Ok::<_, std::io::Error>(status)
    })
    .await;
    let message = match result {
        Ok(Ok(status)) => {
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        Ok(Err(error)) => {
            let message = format!("读取 {name} 执行结果失败（{}）。", error.kind());
            diagnostic::with_detail(message, name, &stderr)
        }
        Err(_) => {
            let message = format!("{name} 执行超过 {} 秒。", deadline.as_secs());
            diagnostic::with_detail(message, name, &stderr)
        }
    };
    let _ = child.kill().await;
    Err(message)
}

pub(super) fn exit_message(name: &str, output: &Output) -> String {
    if database_locked(output) {
        return format!("{name} 重试后仍无法打开已锁定的数据库。");
    }
    let message = match output.status.code() {
        Some(code) => format!("{name} 执行失败（退出码 {code}）。"),
        None => format!("{name} 已中断。"),
    };
    diagnostic::with_detail(message, name, &output.stderr)
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, sync::Arc};

    use serde_json::json;
    use tokio::sync::Notify;

    use super::*;
    use crate::modules::{
        alert::AlertService,
        config::service::ConfigService,
        emacs::{config::EmacsConfig, service::EmacsService},
        notification::NotificationService,
        status::StatusService,
    };

    #[tokio::test]
    async fn refresh_coalesces_requests_while_indexing() {
        let directory = std::env::temp_dir().join(format!("loki-mail-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let client = directory.join("emacsclient");
        let mu = directory.join("mu");
        let database = json!({"program": mu, "home": directory});
        let result = serde_json::to_string(&database.to_string()).unwrap();
        std::fs::write(
            directory.join("ready"),
            serde_json::to_string(&json!({"result": result}).to_string()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            &client,
            r##"#!/bin/sh
if [ -f "$0.busy" ]; then
    printf '%s\n' '"{\"result\":\"busy\"}"'
else
    cat "${0%/*}/ready"
fi
printf 'eval\n' >> "$0.calls"
"##,
        )
        .unwrap();
        std::fs::write(
            &mu,
            "#!/bin/sh\n[ \"$1\" = find ] || exit 1\nprintf '%s\\n' '[{\"message-id\":\"fixture@example.org\"}]'\n",
        )
        .unwrap();
        for path in [&client, &mu] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let busy = client.with_extension("busy");
        let calls = client.with_extension("calls");
        std::fs::write(&busy, "").unwrap();

        let config = Arc::new(ConfigService::from_registry());
        config
            .replace("emacs", "emacsclient_path", json!(client))
            .await
            .unwrap();
        let status = Arc::new(StatusService::new());
        let notification = Arc::new(NotificationService::new());
        let service = Arc::new(MailService {
            emacs: Arc::new(EmacsService::new(
                EmacsConfig::new(Arc::clone(&config)),
                Arc::clone(&notification),
                Arc::clone(&status),
            )),
            alert: Arc::new(AlertService::new(
                Arc::clone(&status),
                Arc::clone(&notification),
            )),
            config,
            notification,
            status,
            refresh_requested: Notify::new(),
            stopping: Notify::new(),
        });
        let task = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.sync_loop().await }
        });
        service.refresh_requested.notify_one();
        time::timeout(Duration::from_secs(2), async {
            while !calls.exists() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        for _ in 0..10 {
            service.refresh_requested.notify_one();
        }
        time::sleep(Duration::from_millis(100)).await;
        let attempted_while_busy = std::fs::read_to_string(&calls).unwrap();
        std::fs::remove_file(busy).unwrap();
        let refreshed = time::timeout(REFRESH_RETRY_INTERVAL + Duration::from_secs(2), async {
            loop {
                if let Ok(status) = service.status.value(MODULE_ID, "unread_count").await {
                    break status.value;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        time::sleep(Duration::from_millis(100)).await;
        task.abort();
        let _ = task.await;
        let attempts = std::fs::read_to_string(calls).unwrap();
        std::fs::remove_dir_all(directory).unwrap();

        assert_eq!(attempted_while_busy, "eval\n");
        assert_eq!(refreshed.unwrap(), json!(1));
        assert_eq!(attempts, "eval\neval\n");
    }

    #[tokio::test]
    async fn retains_mbsync_diagnostics_when_services_times_out() {
        let error = run(
            Command::new("/bin/sh").args([
                "-c",
                "printf '%s\n' 'Cannot connect to imap.example.org (192.0.2.1:993): Connection refused' >&2; exec /bin/sleep 30",
            ]),
            "mbsync",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.contains("mbsync 执行超过 1 秒"));
        assert!(error.contains("连接 imap.example.org:993 被拒绝"));
    }
}
