use std::{env, path::PathBuf, sync::Arc};

use loki_services::{
    Services, http,
    platform::macos::{LaunchdManager, LaunchdServiceSpec},
    runtime::{RuntimeConfig, RuntimeState, ServiceInstanceLock, ServicePfile},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref() {
        #[cfg(feature = "network-probe")]
        Some("network-probe-relay") => {
            loki_services::modules::network_probe::run_relay().await?;
        }
        #[cfg(feature = "network-probe")]
        Some("network-probe") => run_network_probe().await?,
        #[cfg(feature = "network-probe")]
        Some("network-probe-client") => {
            println!(
                "{}",
                loki_services::modules::network_probe::probe::run_mobile().await?
            );
        }
        Some("install-service") => install_service()?,
        Some("uninstall-service") => uninstall_service()?,
        Some("start-service") => start_service()?,
        Some("stop-service") => stop_service()?,
        Some("restart-service") => restart_service()?,
        Some("service-status") => service_status()?,
        Some("--help") | Some("-h") => print_help(),
        Some(command) => {
            return Err(format!("未知命令：{command}，使用 --help 查看支持的命令").into());
        }
        None => run_server().await?,
    }
    Ok(())
}

#[cfg(feature = "network-probe")]
async fn run_network_probe() -> Result<(), Box<dyn std::error::Error>> {
    // 验证实例使用独立的随机 loopback 端口，不占用正式服务的锁和 pfile。
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let runtime = Arc::new(RuntimeState::new(Arc::new(Services::build())));
    eprintln!("验证 services HTTP：http://{}", listener.local_addr()?);
    tokio::select! {
        result = http::serve(listener, runtime) => result?,
        result = loki_services::modules::network_probe::probe::run_services() => result?,
        _ = tokio::signal::ctrl_c() => {},
    }
    Ok(())
}

async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_config = RuntimeConfig::from_env()?;
    let _instance_lock = ServiceInstanceLock::acquire()?;
    let listener = tokio::net::TcpListener::bind(runtime_config.http_addr).await?;
    let service_pfile = ServicePfile::for_listener(listener.local_addr()?);
    let services = Arc::new(Services::load()?);
    let service_path = service_pfile.write()?;
    let runtime = Arc::new(RuntimeState::new(services));

    eprintln!(
        "loki-services listening on http://{} (health: /healthz)",
        listener.local_addr()?
    );

    tokio::select! {
        result = http::serve(listener, runtime) => result?,
        _ = tokio::signal::ctrl_c() => {}
    }
    service_pfile.remove_if_owner()?;
    eprintln!("services information removed: {}", service_path.display());
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

fn start_service() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    LaunchdManager::new()?.start(&spec)?;
    println!("已启动 LaunchAgent：{}", spec.label);
    Ok(())
}

fn stop_service() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    LaunchdManager::new()?.stop(&spec)?;
    println!("已停止 LaunchAgent：{}", spec.label);
    Ok(())
}

fn restart_service() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    LaunchdManager::new()?.restart(&spec)?;
    println!("已重启 LaunchAgent：{}", spec.label);
    Ok(())
}

fn service_status() -> Result<(), Box<dyn std::error::Error>> {
    let spec = service_spec()?;
    println!("{:?}", LaunchdManager::new()?.status(&spec)?);
    Ok(())
}

fn service_spec() -> Result<LaunchdServiceSpec, Box<dyn std::error::Error>> {
    let executable = env::current_exe()?;
    let mut spec = LaunchdServiceSpec::new("cc.loki-assistant.services", executable);
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
    println!("  start-service      启动用户级 LaunchAgent");
    println!("  stop-service       停止用户级 LaunchAgent");
    println!("  restart-service    重启用户级 LaunchAgent");
    println!("  service-status     查询用户级 LaunchAgent 状态");
    #[cfg(feature = "network-probe")]
    println!("  network-probe / network-probe-relay  启动本地身份组网验证节点或中继");
    #[cfg(feature = "network-probe")]
    println!("  network-probe-client  使用手机身份执行一次双向验证");
}
