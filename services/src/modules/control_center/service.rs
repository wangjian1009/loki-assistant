use std::{path::PathBuf, sync::Arc};

use crate::{
    modules::config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler},
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

impl<R: CommandRunner + std::fmt::Debug + 'static> ConfigChangeHandler for ControlCenterService<R> {
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        self.apply(change, false)
    }

    fn rollback(&self, change: ConfigChange) -> ChangeFuture<'_> {
        self.apply(change, true)
    }
}

impl<R: CommandRunner + 'static> ControlCenterService<R> {
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
            self.process
                .blocking(move |(manager, spec)| {
                    manager
                        .set_login_startup(spec, enabled)
                        .map_err(|error| error.to_string())
                })
                .await
        })
    }
}

/// 构造平台适配失败仍允许服务启动，但提交时明确返回错误。
#[derive(Debug)]
pub(crate) struct UnavailableHandler(pub String);

impl ConfigChangeHandler for UnavailableHandler {
    fn handle(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { Err(self.0.clone()) })
    }
}

pub(crate) fn handler() -> Arc<dyn ConfigChangeHandler> {
    match ControlCenterService::new() {
        Ok(service) => Arc::new(service),
        Err(message) => Arc::new(UnavailableHandler(message)),
    }
}
