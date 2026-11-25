use std::{env, io, net::SocketAddr, sync::Arc, time::Duration};

use loki_services::{
    Services, http,
    logging::LogWorker,
    platform::macos::notification::show_without_receipt,
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
    let _instance_lock = ServiceInstanceLock::acquire()?;
    let log_path = ServicePfile::path()?
        .with_file_name("logs")
        .join("services.log");
    let _log_worker = LogWorker::start(log_path)?;
    let result = serve().await;
    if let Err(error) = &result {
        log::error!("services 运行失败：{error}");
    }
    result
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
    let services = Arc::new(Services::load()?);
    let runtime_config = RuntimeConfig::from_env(services.configured_http_port().await?)?;
    let listener = bind_http(runtime_config.http_addr, notify_port_conflict).await?;
    let address = listener.local_addr()?;
    let service_pfile = ServicePfile::for_listener(address);
    let config = services.config.service();
    if let Err(error) = config.migrate_pending_password_paths().await {
        log::warn!("迁移待确认配置的密码路径失败：{error}");
    }
    let service_path = service_pfile.write()?;
    let runtime = Arc::new(RuntimeState::new(Arc::clone(&services)));
    services.attach_remote(&runtime);
    let extensions = services.start_extensions(address.port());
    let notification = Arc::clone(&services.notification);
    let notification_retry = tokio::spawn(async move { notification.retry_pending().await });
    let network = Arc::clone(&services.network);
    let network_start = tokio::spawn(async move {
        if let Err(error) = network.start().await {
            log::error!("网络初始化失败：{error}");
        }
    });

    log::info!("services 开始监听 http://{address}");

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = tokio::select! {
        result = http::serve(listener, runtime) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = terminate.recv() => Ok(())
    };
    notification_retry.abort();
    let _ = notification_retry.await;
    network_start.abort();
    services.stop_extensions(extensions).await;
    let _ = network_start.await;
    services.network.shutdown().await;
    let usage_saved = services.flush_llm_usage().await;
    service_pfile.remove_if_owner()?;
    result?;
    if !usage_saved {
        return Err("无法保存 LLM 用量明细。".into());
    }
    log::info!(
        "services 已停止，已移除服务信息文件 {}",
        service_path.display()
    );
    Ok(())
}

async fn bind_http(
    address: SocketAddr,
    notify_conflict: impl AsyncFnOnce(SocketAddr),
) -> io::Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(address).await {
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            notify_conflict(address).await;
            Err(error)
        }
        result => result,
    }
}

async fn notify_port_conflict(address: SocketAddr) {
    log::warn!("HTTP 监听地址 {address} 已被占用");
    let body = format!("无法监听 {address}。请释放该端口，或调整 services 的启动地址后重新启动。");
    match tokio::time::timeout(
        Duration::from_secs(3),
        show_without_receipt("Loki services 监听端口被占用", &body),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => log::warn!("端口冲突通知发送失败：{error}"),
        Err(_) => log::warn!("端口冲突通知发送超时"),
    }
}

fn print_help() {
    println!("loki-services");
    println!("  无参数       启动本地 HTTP 服务");
    println!("  服务管理     使用 brew services 或根目录 Makefile");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn occupied_port_notifies_and_binding_succeeds_after_release() {
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = occupied.local_addr().unwrap();
        let mut notified = None;
        let error = bind_http(address, async |address| notified = Some(address))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(notified, Some(address));

        drop(occupied);
        let listener = bind_http(address, async |_| panic!("不应发送冲突通知"))
            .await
            .unwrap();
        assert_eq!(listener.local_addr().unwrap(), address);
        assert_eq!(
            ServicePfile::for_listener(listener.local_addr().unwrap()).port,
            address.port()
        );
    }
}
