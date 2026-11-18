//! 本地身份组网验证，仅由显式启用的命令启动，不注册业务 HTTP API。

pub mod probe;

pub async fn run_relay() -> anyhow::Result<()> {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(
        "127.0.0.1:3340".parse::<std::net::SocketAddr>()?,
    ));
    let mut server = Server::spawn(config).await?;
    println!("本地 iroh-relay：http://127.0.0.1:3340（仅 loopback 验证）");
    tokio::select! {
        result = server.join() => { result??; }
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}
