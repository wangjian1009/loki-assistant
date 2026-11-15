use std::{fmt::Debug, future::Future, pin::Pin};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;

/// 菜单栏应用承接的系统通知数据；buffer 暂时仅作为元数据保留。
#[derive(Clone, Debug, Serialize)]
pub struct NotificationContent {
    pub title: String,
    pub icon: Option<String>,
    pub buffer: String,
}

pub type NotificationFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

pub trait NotificationLauncher: Debug + Send + Sync {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_>;
}

#[derive(Debug)]
pub struct MacNotificationLauncher;

impl NotificationLauncher for MacNotificationLauncher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        Box::pin(async move {
            #[cfg(target_os = "macos")]
            {
                let output = tokio::process::Command::new("/usr/bin/open")
                    .args(["-g", &notification_url(&content)?])
                    .kill_on_drop(true)
                    .output()
                    .await
                    .map_err(|error| format!("无法唤起菜单栏应用：{error}"))?;
                if output.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "无法转交系统通知（退出码 {:?}）：{}；请先运行 Loki Assistant 菜单栏应用",
                        output.status.code(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = content;
                Err("当前平台不支持 macOS 系统通知".to_owned())
            }
        })
    }
}

fn notification_url(content: &NotificationContent) -> Result<String, String> {
    let json = serde_json::to_string(content).map_err(|error| error.to_string())?;
    Ok(format!(
        "loki-assistant://emacs/notification?payload={}",
        utf8_percent_encode(&json, NON_ALPHANUMERIC)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_notification_as_a_single_url_argument() {
        let content = NotificationContent {
            title: "中文 & \"通知\"\n$(test)".to_owned(),
            icon: Some("/tmp/图标 #1.png".to_owned()),
            buffer: "*scratch*".to_owned(),
        };
        let url = notification_url(&content).unwrap();
        let encoded = url
            .strip_prefix("loki-assistant://emacs/notification?payload=")
            .unwrap();
        let decoded = percent_encoding::percent_decode_str(encoded)
            .decode_utf8()
            .unwrap();
        assert_eq!(decoded, serde_json::to_string(&content).unwrap());
        assert!(!encoded.contains([' ', '\n', '&', '#', '"']));
    }
}
