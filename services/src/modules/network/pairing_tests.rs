use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use iroh::{EndpointAddr, SecretKey};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    pairing::PairingService,
    protocol::{BindRequest, PAIRING_ALPN, PairingStatus},
    tests::Fixture,
};

fn request(id: &str, token: &str) -> BindRequest {
    BindRequest {
        api_version: "1".to_owned(),
        kind: "pairing.bind".to_owned(),
        pairing_id: id.to_owned(),
        pairing_token: token.to_owned(),
        device_name: "我的手机".to_owned(),
    }
}

#[tokio::test]
async fn services_png_decodes_to_the_exact_invitation_and_keeps_a_quiet_zone() {
    let fixture = Fixture::new();
    let manager = PairingService::new(Ok(fixture.root.join("peers.json")));
    let created = manager
        .create(
            SecretKey::generate().public().to_string(),
            "https://relay.example:8443/".into(),
            "中继令牌+/=\"\\".into(),
        )
        .await
        .unwrap();
    let png = STANDARD.decode(&created.qr_code_png_base64).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Grayscale);
    assert_eq!(info.width, info.height);
    let width = info.width as usize;
    assert!(pixels[..width * 24].iter().all(|pixel| *pixel == 255));
    let mut decoder = quircs::Quirc::default();
    let decoded = decoder
        .identify(width, width, &pixels[..info.buffer_size()])
        .next()
        .unwrap()
        .unwrap()
        .decode()
        .unwrap();
    let value: Value = serde_json::from_slice(&decoded.payload).unwrap();
    assert_eq!(value, serde_json::to_value(&created.qr_payload).unwrap());
    let failed = manager
        .create(
            SecretKey::generate().public().to_string(),
            "https://relay.example/".into(),
            "x".repeat(4000),
        )
        .await
        .err()
        .unwrap();
    assert_eq!(failed.code(), "pairing-qr-code-error");
    assert_eq!(
        manager
            .query(&created.pairing_id, false)
            .await
            .unwrap()
            .status,
        PairingStatus::Pending
    );
}

#[tokio::test]
async fn invitations_expire_refresh_cancel_and_reject_wrong_tokens() {
    let fixture = Fixture::new();
    let manager = Arc::new(PairingService::new(Ok(fixture.root.join("peers.json"))));
    let server = SecretKey::generate().public().to_string();
    let first = manager
        .create(
            server.clone(),
            "https://relay.example".into(),
            "relay-secret".into(),
        )
        .await
        .unwrap();
    assert_eq!(first.qr_payload.pairing_token.len(), 64);
    assert!((299..=300).contains(&(first.qr_payload.expires_at - super::pairing::now())));
    let phone = SecretKey::generate().public();
    assert_eq!(
        manager
            .bind(phone, request(&first.pairing_id, &"0".repeat(64)))
            .await
            .unwrap_err()
            .code(),
        "invalid-pairing-token"
    );
    let second = manager
        .create(
            server,
            "https://relay.example".into(),
            "relay-secret".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        manager
            .query(&first.pairing_id, false)
            .await
            .unwrap()
            .status,
        PairingStatus::Cancelled
    );
    assert_eq!(
        manager
            .bind(
                phone,
                request(&first.pairing_id, &first.qr_payload.pairing_token)
            )
            .await
            .unwrap_err()
            .code(),
        "pairing-cancelled"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(300)).await;
    assert_eq!(
        manager
            .query(&second.pairing_id, false)
            .await
            .unwrap()
            .status,
        PairingStatus::Expired
    );
    assert_eq!(
        manager
            .bind(
                phone,
                request(&second.pairing_id, &second.qr_payload.pairing_token)
            )
            .await
            .unwrap_err()
            .code(),
        "pairing-expired"
    );
}

#[tokio::test]
async fn concurrent_binding_has_one_winner_and_retries_are_idempotent() {
    let fixture = Fixture::new();
    let path = fixture.root.join("peers.json");
    let manager = Arc::new(PairingService::new(Ok(path.clone())));
    let created = manager
        .create(
            SecretKey::generate().public().to_string(),
            "https://relay.example".into(),
            "relay-secret".into(),
        )
        .await
        .unwrap();
    let first = SecretKey::generate().public();
    let second = SecretKey::generate().public();
    let bind = || request(&created.pairing_id, &created.qr_payload.pairing_token);
    let (a, b) = tokio::join!(manager.bind(first, bind()), manager.bind(second, bind()));
    assert_ne!(a.is_ok(), b.is_ok());
    let winner = if a.is_ok() { first } else { second };
    let loser = if a.is_ok() {
        b.unwrap_err()
    } else {
        a.unwrap_err()
    };
    assert_eq!(loser.code(), "pairing-consumed");
    let progress = manager.query(&created.pairing_id, false).await.unwrap();
    let retry = manager.bind(winner, bind()).await.unwrap();
    assert_eq!(retry.bound_at, progress.peer.unwrap().bound_at);
    assert_eq!(
        manager
            .query(&created.pairing_id, true)
            .await
            .unwrap_err()
            .code(),
        "pairing-completed"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stored = fs::read_to_string(&path).unwrap();
    assert!(!stored.contains("relay-secret"));
    assert!(!stored.contains(&created.qr_payload.pairing_token));
    let restarted = PairingService::new(Ok(path));
    assert_eq!(
        restarted.list().await.unwrap()[0].endpoint_id,
        winner.to_string()
    );
    assert!(restarted.query(&created.pairing_id, false).await.is_err());
    manager.remove(winner).await.unwrap();
    assert!(manager.list().await.unwrap().is_empty());
    assert_eq!(
        manager.bind(winner, bind()).await.unwrap_err().code(),
        "pairing-cancelled"
    );
    assert_eq!(
        manager.remove(winner).await.unwrap_err().code(),
        "peer-not-found"
    );
}

#[tokio::test]
async fn storage_failure_keeps_invitation_available_and_corrupt_records_are_preserved() {
    let fixture = Fixture::new();
    let path = fixture.root.join("peers.json");
    let manager = Arc::new(PairingService::new(Ok(path.clone())));
    let created = manager
        .create(
            SecretKey::generate().public().to_string(),
            "https://relay.example".into(),
            "relay-secret".into(),
        )
        .await
        .unwrap();
    let phone = SecretKey::generate().public();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        manager
            .bind(
                phone,
                request(&created.pairing_id, &created.qr_payload.pairing_token)
            )
            .await
            .unwrap_err()
            .code(),
        "peer-storage-error"
    );
    assert_eq!(
        manager
            .query(&created.pairing_id, false)
            .await
            .unwrap()
            .status,
        PairingStatus::Pending
    );
    fs::remove_dir(&path).unwrap();
    manager
        .bind(
            phone,
            request(&created.pairing_id, &created.qr_payload.pairing_token),
        )
        .await
        .unwrap();
    let backup = fixture.root.join("peers.backup");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(
        manager.remove(phone).await.unwrap_err().code(),
        "peer-storage-error"
    );
    assert_eq!(manager.list().await.unwrap().len(), 1);
    fs::remove_dir(&path).unwrap();
    fs::rename(backup, &path).unwrap();
    fs::write(&path, "损坏的记录").unwrap();
    assert!(PairingService::new(Ok(path.clone())).list().await.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "损坏的记录");
    let link = fixture.root.join("linked.json");
    symlink(&path, &link).unwrap();
    assert!(PairingService::new(Ok(link)).list().await.is_err());
}

async fn http(app: axum::Router, method: &str, path: &str) -> (u16, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let status = response.status().as_u16();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    (status, value)
}

#[tokio::test]
async fn http_lifecycle_uses_committed_settings_and_never_returns_tokens_in_queries() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    let app = service.router();
    let (status, error) = http(app.clone(), "POST", "/network/pairings").await;
    assert_eq!(status, 503);
    assert_eq!(error["error"]["code"], "network-not-ready");
    config
        .replace("network", "relay_url", json!("https://relay.example"))
        .await
        .unwrap();
    config
        .replace("network", "relay_token", json!("relay-secret"))
        .await
        .unwrap();
    let (status, created) = http(app.clone(), "POST", "/network/pairings").await;
    assert_eq!(status, 201);
    assert_eq!(created["qr_payload"]["relay_token"], "relay-secret");
    let path = format!(
        "/network/pairings/{}",
        created["pairing_id"].as_str().unwrap()
    );
    let (status, progress) = http(app.clone(), "GET", &path).await;
    assert_eq!(status, 200);
    assert_eq!(progress["status"], "pending");
    assert!(!progress.to_string().contains("token"));
    fixture
        .registrar
        .fail
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        config
            .replace("network", "relay_token", json!("rejected-token"))
            .await
            .is_err()
    );
    assert_eq!(http(app.clone(), "GET", &path).await.1["status"], "pending");
    fixture
        .registrar
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    config
        .replace("network", "relay_token", json!("new-token"))
        .await
        .unwrap();
    assert_eq!(
        http(app.clone(), "GET", &path).await.1["status"],
        "cancelled"
    );
    assert_eq!(http(app.clone(), "DELETE", &path).await.0, 200);
    assert_eq!(
        http(app.clone(), "GET", "/network/peers").await.1["peers"],
        json!([])
    );
    assert_eq!(http(app.clone(), "PUT", "/network/peers").await.0, 405);
    assert_eq!(
        http(app.clone(), "GET", "/network/pairings/invalid")
            .await
            .0,
        400
    );
    service.shutdown().await;
}

#[tokio::test]
async fn services_mounts_all_five_network_apis_without_exposing_credentials() {
    let services = Arc::new(crate::Services::build());
    let app = services.router(Arc::new(crate::runtime::RuntimeState::new(
        services.clone(),
    )));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/discover/modules/network")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let metadata: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65_536).await.unwrap()).unwrap();
    assert_eq!(metadata["apis"].as_array().unwrap().len(), 5);
    assert!(!metadata.to_string().contains("relay_token"));
    assert_eq!(http(app, "POST", "/network/pairings").await.0, 503);
}

#[tokio::test]
async fn real_relay_handshake_authenticates_phone_identity_and_persists_binding() {
    use super::{IrohRelayRegistrar, NetworkService, RelayRegistrar};
    use iroh_relay::server::{
        Access, AccessControl, ClientRequest, RelayConfig, Server, ServerConfig,
    };
    #[derive(Debug)]
    struct TokenAccess(String);
    impl AccessControl for TokenAccess {
        async fn on_connect(&self, request: &ClientRequest) -> Access {
            if request.auth_token().as_deref() == Some(self.0.as_str()) {
                Access::Allow
            } else {
                Access::Deny {
                    reason: Some("令牌不匹配".into()),
                }
            }
        }
    }
    let fixture = Fixture::new();
    let token = uuid::Uuid::new_v4().to_string();
    let mut relay_config = RelayConfig::new("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap());
    relay_config.access = Arc::new(TokenAccess(token.clone()));
    let mut server_config = ServerConfig::default();
    server_config.relay = Some(relay_config);
    let relay = Server::spawn(server_config).await.unwrap();
    let address = format!("http://{}", relay.http_addr().unwrap());
    let service = Arc::new(NetworkService::new(
        Ok(fixture.store()),
        Arc::new(IrohRelayRegistrar::new()),
        Arc::new(
            crate::modules::notification::NotificationService::with_launcher(
                fixture.launcher.clone(),
                Duration::from_secs(1),
            ),
        ),
        fixture.status.clone(),
        Duration::from_secs(10),
    ));
    let receiver = tokio::spawn(service.clone().receive_pairings());
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_url", json!(address))
        .await
        .unwrap();
    config
        .replace("network", "relay_token", json!(token))
        .await
        .unwrap();
    let created = service.create_pairing().await.unwrap();
    let phone_key = SecretKey::generate();
    let phone_id = phone_key.public();
    let phone = IrohRelayRegistrar::new()
        .register(phone_key, address.clone(), token)
        .await
        .unwrap();
    let endpoint = phone.endpoint().unwrap();
    let remote = EndpointAddr::new(created.qr_payload.endpoint_id.parse().unwrap())
        .with_relay_url(address.parse().unwrap());
    let exchange = |body: Vec<u8>| {
        let endpoint = endpoint.clone();
        let remote = remote.clone();
        async move {
            let connection = endpoint.connect(remote, PAIRING_ALPN).await.unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.write_all(&body).await.unwrap();
            send.finish().unwrap();
            let data = recv.read_to_end(16_384).await.unwrap();
            serde_json::from_slice::<Value>(&data).unwrap()
        }
    };
    let bad = serde_json::to_vec(&request(&created.pairing_id, &"0".repeat(64))).unwrap();
    let rejected = tokio::time::timeout(Duration::from_secs(10), exchange(bad))
        .await
        .unwrap();
    assert_eq!(rejected["status"], 403);
    let good = serde_json::to_vec(&request(
        &created.pairing_id,
        &created.qr_payload.pairing_token,
    ))
    .unwrap();
    let mut spoofed: Value = serde_json::from_slice(&good).unwrap();
    spoofed["endpoint_id"] = json!(SecretKey::generate().public().to_string());
    let rejected = tokio::time::timeout(
        Duration::from_secs(10),
        exchange(serde_json::to_vec(&spoofed).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(rejected["status"], 400);
    let response = tokio::time::timeout(Duration::from_secs(10), exchange(good))
        .await
        .unwrap();
    assert_eq!(response["type"], "pairing.bound");
    assert_eq!(
        service.peers().await.unwrap()[0].endpoint_id,
        phone_id.to_string()
    );
    assert_eq!(
        service
            .pairing(&created.pairing_id, false)
            .await
            .unwrap()
            .status,
        PairingStatus::Completed
    );
    let (status, removed) = http(
        service.router(),
        "DELETE",
        &format!("/network/peers/{phone_id}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(removed["removed"], true);
    assert!(service.peers().await.unwrap().is_empty());
    receiver.abort();
    let _ = receiver.await;
    service.shutdown().await;
    phone.close().await.unwrap();
    relay.shutdown().await.unwrap();
}
