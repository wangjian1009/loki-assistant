use std::sync::Arc;

use loki_services::{
    Services, http,
    runtime::{RuntimeConfig, RuntimeState},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
