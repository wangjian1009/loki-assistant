#[cfg(test)]
use crate::modules::config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler};
use std::{path::PathBuf, sync::Arc};

use crate::{
    platform::macos::{CommandRunner, LaunchdManager, LaunchdServiceSpec},
    process::Process,
};

/// 控制中心只在配置提交时同步系统设置，构造与服务启动不修改 LaunchAgent。
#[derive(Debug)]
pub struct ControlCenterService<R = crate::platform::macos::ProcessCommandRunner> {
    process: Process<(LaunchdManager<R>, LaunchdServiceSpec)>,
}

impl ControlCenterService {
    pub fn new() -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("登录启动仅支持 macOS".to_owned());
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "无法确定用户主目录".to_owned())?;
        let executable = std::env::var_os("LOKI_ASSISTANT_CENTER_EXECUTABLE").map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../apps/center/mac/.build/Debug/LokiAssistant.app/Contents/MacOS/LokiAssistant"));
        Ok(Self::with_manager(
            LaunchdManager::new().map_err(|error| error.to_string())?,
            executable,
            home,
        ))
    }
}

impl<R: CommandRunner + 'static> ControlCenterService<R> {
    pub fn with_manager(manager: LaunchdManager<R>, executable: PathBuf, home: PathBuf) -> Self {
        let mut spec = LaunchdServiceSpec::new("cc.loki-assistant.center", executable);
        spec.keep_alive = false;
        spec.session_type = Some("Aqua".to_owned());
        spec.stdout_path = Some(home.join("Library/Logs/LokiAssistant/center.log"));
        spec.stderr_path = Some(home.join("Library/Logs/LokiAssistant/center.error.log"));
        Self {
            process: Process::start((manager, spec)),
        }
    }
}

#[cfg(test)]
impl<R: CommandRunner + std::fmt::Debug + 'static> ConfigChangeHandler for ControlCenterService<R> {
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        self.apply(change, false)
    }

    fn rollback(&self, change: ConfigChange) -> ChangeFuture<'_> {
        self.apply(change, true)
    }
}

impl<R: CommandRunner + 'static> ControlCenterService<R> {
    #[cfg(test)]
    fn apply(&self, change: ConfigChange, rollback: bool) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.module_id != "control-center" || change.key != "center_auto_start" {
                return Ok(());
            }
            let value = if rollback {
                change.previous_value
            } else {
                Some(change.value)
            };
            let enabled = value
                .and_then(|value| value.as_bool())
                .ok_or_else(|| "登录启动配置必须是 boolean".to_owned())?;
            self.set_enabled(enabled).await
        })
    }
    async fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.process
            .blocking(move |(manager, spec)| {
                manager
                    .set_login_startup(spec, enabled)
                    .map_err(|error| error.to_string())
            })
            .await
    }
}

pub(crate) fn notification_router() -> axum::Router {
    notification_router_with(ControlCenterService::new().map(Arc::new))
}

pub(super) fn notification_router_with<R: CommandRunner + 'static>(
    service: Result<Arc<ControlCenterService<R>>, String>,
) -> axum::Router {
    use axum::routing::post;
    axum::Router::new()
        .route(
            &format!("{}/{{value}}", super::config::NOTIFICATION),
            post(notify_write::<R>).delete(notify_delete::<R>),
        )
        .with_state(service)
}

type Platform<R> = Result<Arc<ControlCenterService<R>>, String>;

async fn notify_write<R: CommandRunner + 'static>(
    axum::extract::State(service): axum::extract::State<Platform<R>>,
    axum::Extension(change): axum::Extension<crate::modules::config::service::NotificationRequest>,
    axum::Json(enabled): axum::Json<bool>,
) -> axum::response::Response {
    notify(service, change, enabled).await
}

async fn notify_delete<R: CommandRunner + 'static>(
    axum::extract::State(service): axum::extract::State<Platform<R>>,
    axum::Extension(change): axum::Extension<crate::modules::config::service::NotificationRequest>,
) -> axum::response::Response {
    notify(service, change, false).await
}

async fn notify<R: CommandRunner + 'static>(
    service: Platform<R>,
    change: crate::modules::config::service::NotificationRequest,
    enabled: bool,
) -> axum::response::Response {
    use crate::modules::config::service::NotificationValidationError;
    use axum::{http::StatusCode, response::IntoResponse};
    let result = match &service {
        Ok(service) => service.set_enabled(enabled).await,
        Err(error) => Err(error.clone()),
    };
    if let Err(message) = result {
        let mut response = StatusCode::UNPROCESSABLE_ENTITY.into_response();
        response
            .extensions_mut()
            .insert(NotificationValidationError {
                code: "center-auto-start".into(),
                message,
                clear_password_fields: Vec::new(),
            });
        return response;
    }
    let previous = change
        .previous
        .as_ref()
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let service = service.expect("平台可用");
    let mut response = StatusCode::NO_CONTENT.into_response();
    change.settle_with(&mut response, move |confirmed| async move {
        if !confirmed && let Err(error) = service.set_enabled(previous).await {
            log::warn!("恢复登录启动配置失败：{error}");
        }
    });
    response
}
