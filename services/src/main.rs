use std::{env, path::PathBuf, sync::Arc};

use loki_services::{
    Services, http,
    platform::macos::{LaunchdManager, LaunchdServiceSpec},
    runtime::{RuntimeConfig, RuntimeState},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref() {
        Some("install-service") => install_service()?,
        Some("uninstall-service") => uninstall_service()?,
        Some("--help") | Some("-h") => print_help(),
        Some(command) => {
            return Err(format!("未知命令：{command}，使用 --help 查看支持的命令").into());
        }
        None => run_server().await?,
    }
    Ok(())
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_config = RuntimeConfig::from_env()?;
    let listener = tokio::net::TcpListener::bind(runtime_config.http_addr).await?;
    let services = Arc::new(Services::build());
    let runtime = Arc::new(RuntimeState::new(services));

    eprintln!(
        "loki-services listening on http://{} (health: /healthz)",
        listener.local_addr()?
    );

    tokio::select! {
        result = http::serve(listener, runtime) => result?,
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

fn install_service() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    let path = LaunchdManager::new()?.install(&spec)?;
    println!("已注册 LaunchAgent：{}", path.display());
    Ok(())
}

fn uninstall_service() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    LaunchdManager::new()?.uninstall(&spec)?;
    println!("已取消注册 LaunchAgent：{}", spec.label);
    Ok(())
}

fn service_spec() -> Result<LaunchdServiceSpec, Box<dyn std::error::Error>> {
    let executable = env::current_exe()?;
    let mut spec = LaunchdServiceSpec::new("personal.loki-assistant.services", executable);
    if let Some(address) = env::var_os("LOKI_ASSISTANT_HTTP_ADDR") {
        spec.environment.insert(
            "LOKI_ASSISTANT_HTTP_ADDR".to_owned(),
            address.to_string_lossy().into_owned(),
        );
    }
    if let Some(home) = env::var_os("HOME") {
        let log_directory = PathBuf::from(home).join("Library/Logs/LokiAssistant");
        spec.stdout_path = Some(log_directory.join("services.log"));
        spec.stderr_path = Some(log_directory.join("services.error.log"));
    }
    Ok(spec)
}

fn print_help() {
    println!("loki-services");
    println!("  无参数       启动本地 HTTP 服务");
    println!("  install-service   注册当前可执行文件为用户级 LaunchAgent");
    println!("  uninstall-service 取消注册用户级 LaunchAgent");
}
