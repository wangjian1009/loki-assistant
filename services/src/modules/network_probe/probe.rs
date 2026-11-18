// services 与手机原生库共同编译此验证协议，避免维护两套身份和消息约定。
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMap, RelayMode, RelayUrl, SecretKey, endpoint::presets,
};
use serde_json::{Value, json};
use tokio::time::timeout;

pub const ALPN: &[u8] = b"loki-assistant/local-probe/1";
pub const RELAY_URL: &str = "http://127.0.0.1:3340";
fn relay_url() -> String {
    std::env::var("LOKI_ASSISTANT_RELAY_URL").unwrap_or_else(|_| {
        option_env!("LOKI_ASSISTANT_RELAY_URL")
            .unwrap_or(RELAY_URL)
            .to_owned()
    })
}

fn relay_token() -> Option<String> {
    std::env::var("LOKI_ASSISTANT_RELAY_TOKEN")
        .ok()
        .or_else(|| option_env!("LOKI_ASSISTANT_RELAY_TOKEN").map(str::to_owned))
}
const LIMIT: usize = 4096;
const BUDGET: Duration = Duration::from_secs(30);
// 公开测试密钥，仅供本地验证，不能用于真实账号或生产部署。
const SERVICES_SEED: [u8; 32] = [0x11; 32];
const MOBILE_SEED: [u8; 32] = [0x22; 32];

fn identity(mobile: bool) -> SecretKey {
    SecretKey::from_bytes(if mobile { &MOBILE_SEED } else { &SERVICES_SEED })
}

pub fn peer_id(mobile: bool) -> EndpointId {
    identity(mobile).public()
}

async fn endpoint(mobile: bool) -> Result<Endpoint> {
    let relay: RelayUrl = relay_url().parse()?;
    let relays: RelayMap = [relay].into_iter().collect();
    let relays = match relay_token() {
        Some(token) => relays.with_auth_token(token),
        None => relays,
    };
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(identity(mobile))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Custom(relays))
        // 禁止 IP 直连，保证验证流量确实经过本地中继。
        .clear_ip_transports()
        .bind()
        .await?;
    timeout(BUDGET, endpoint.online())
        .await
        .context("中继连接超时（30 秒）")?;
    ensure!(
        endpoint.addr().ip_addrs().next().is_none(),
        "验证节点不得使用 IP 直连"
    );
    Ok(endpoint)
}

fn message(kind: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"api_version":"1", "request_id":"local-probe-1", "kind":kind}))
        .unwrap()
}

fn check(data: &[u8], kind: &str) -> Result<()> {
    let actual: Value = serde_json::from_slice(data)?;
    let expected: Value = serde_json::from_slice(&message(kind))?;
    ensure!(actual == expected, "消息内容不匹配：{actual}");
    Ok(())
}

pub async fn run_services() -> Result<()> {
    let endpoint = endpoint(false).await?;
    println!("SERVICES_READY {} relay-only", endpoint.id());
    while let Some(incoming) = endpoint.accept().await {
        let result = timeout(BUDGET, async {
            let connection = incoming.await?;
            ensure!(connection.remote_id() == peer_id(true), "拒绝未配对的节点");
            let (mut send, mut recv) = connection.accept_bi().await?;
            check(&recv.read_to_end(LIMIT).await?, "mobile-hello")?;
            send.write_all(&message("services-received")).await?;
            send.finish()?;
            // services 主动发起另一条双向 stream，由手机回复。
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(&message("services-hello")).await?;
            send.finish()?;
            check(&recv.read_to_end(LIMIT).await?, "mobile-received")?;
            let report = report();
            let mut send = connection.open_uni().await?;
            send.write_all(&serde_json::to_vec(&report)?).await?;
            send.finish()?;
            send.stopped().await?;
            println!("PROBE_PASS {report}");
            Ok::<_, anyhow::Error>(())
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("PROBE_FAIL {error:#}"),
            Err(error) => eprintln!("PROBE_FAIL 验证超时：{error}"),
        }
    }
    endpoint.close().await;
    Ok(())
}

fn report() -> Value {
    json!({"ok":true, "transport":"relay-only", "relay":relay_url(),
        "services_id":peer_id(false).to_string(), "mobile_id":peer_id(true).to_string(),
        "mobile_to_services":"mobile-hello → services-received",
        "services_to_mobile":"services-hello → mobile-received"})
}

pub async fn run_mobile() -> Result<String> {
    let endpoint = endpoint(true).await?;
    let result = timeout(BUDGET, async {
        let address = EndpointAddr::new(peer_id(false)).with_relay_url(relay_url().parse()?);
        let connection = endpoint.connect(address, ALPN).await?;
        ensure!(
            connection.remote_id() == peer_id(false),
            "services 身份不匹配"
        );
        let (mut send, mut recv) = connection.open_bi().await?;
        send.write_all(&message("mobile-hello")).await?;
        send.finish()?;
        check(&recv.read_to_end(LIMIT).await?, "services-received")?;
        let (mut send, mut recv) = connection.accept_bi().await?;
        check(&recv.read_to_end(LIMIT).await?, "services-hello")?;
        send.write_all(&message("mobile-received")).await?;
        send.finish()?;
        let mut recv = connection.accept_uni().await?;
        let report: Value = serde_json::from_slice(&recv.read_to_end(LIMIT).await?)?;
        ensure!(report == self::report(), "services 验证结果不一致");
        Ok::<_, anyhow::Error>(report.to_string())
    })
    .await;
    endpoint.close().await;
    match result {
        Ok(result) => result,
        Err(_) => bail!("30 秒内未完成双向消息验证"),
    }
}
