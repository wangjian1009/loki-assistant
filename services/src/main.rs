use std::{env, sync::Arc};

use loki_services::{
    Services, http,
    runtime::{RuntimeConfig, RuntimeState, ServiceInstanceLock, ServicePfile},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref() {
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
    let _instance_lock = ServiceInstanceLock::acquire()?;
    let listener = tokio::net::TcpListener::bind(runtime_config.http_addr).await?;
    let service_pfile = ServicePfile::for_listener(listener.local_addr()?);
    let services = Arc::new(Services::load()?);
    let service_path = service_pfile.write()?;
    let runtime = Arc::new(RuntimeState::new(Arc::clone(&services)));
    services.attach_remote(&runtime);
    let extensions = services.start_extensions(listener.local_addr()?.port());
    let notifications_config = services.config.service();
    let config_notifications = tokio::spawn(async move {
        notifications_config.monitor_notifications().await;
    });
    let network = Arc::clone(&services.network);
    let config = services.config.service();
    let network_start = tokio::spawn(async move {
        if let Err(error) = network.start(config).await {
            eprintln!("网络初始化失败：{error}");
        }
        network.monitor().await;
    });
    let network_pairings = tokio::spawn(services.network.clone().receive_pairings());

    eprintln!(
        "loki-services listening on http://{} (health: /healthz)",
        listener.local_addr()?
    );

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        result = http::serve(listener, runtime) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = terminate.recv() => Ok(())
    };
    network_start.abort();
    network_pairings.abort();
    config_notifications.abort();
    services.stop_extensions(extensions).await;
    let _ = network_start.await;
    let _ = network_pairings.await;
    let _ = config_notifications.await;
    services.network.shutdown().await;
    service_pfile.remove_if_owner()?;
    result?;
    eprintln!("services information removed: {}", service_path.display());
    Ok(())
}

fn print_help() {
    println!("loki-services");
    println!("  无参数       启动本地 HTTP 服务");
    println!("  服务管理     使用 brew services 或根目录 Makefile");
}
