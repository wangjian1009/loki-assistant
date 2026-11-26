use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};

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
        device_type: super::protocol::DeviceType::Mobile,
    }
}

#[tokio::test]
async fn services_png_decodes_to_the_exact_invitation_and_keeps_a_quiet_zone() {
    let fixture = Fixture::new();
    let manager = PairingService::new(fixture.stored_config());
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
    let manager = Arc::new(PairingService::new(fixture.stored_config()));
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
        "pairing-expired"
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
async fn concurrent_binding_consumes_invitation_and_later_attempts_expire() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    let manager = Arc::new(PairingService::new(fixture.stored_config()));
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
    assert_eq!(loser.code(), "pairing-expired");
    let progress = manager.query(&created.pairing_id, false).await.unwrap();
    assert_eq!(progress.status, PairingStatus::Completed);
    assert_eq!(progress.peer.unwrap().endpoint_id, winner.to_string());
    assert_eq!(
        manager.bind(winner, bind()).await.unwrap_err().code(),
        "pairing-expired"
    );
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
    let restarted = PairingService::new(fixture.stored_config());
    assert_eq!(
        restarted.list().await.unwrap()[0].endpoint_id,
        winner.to_string()
    );
    assert!(restarted.query(&created.pairing_id, false).await.is_err());
    manager.remove(winner).await.unwrap();
    assert!(manager.list().await.unwrap().is_empty());
    assert_eq!(
        manager.bind(winner, bind()).await.unwrap_err().code(),
        "pairing-expired"
    );
    assert_eq!(
        manager.remove(winner).await.unwrap_err().code(),
        "peer-not-found"
    );
}

#[tokio::test]
async fn storage_failure_keeps_invitation_available_and_corrupt_records_are_preserved() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    let manager = Arc::new(PairingService::new(fixture.stored_config()));
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
    let corrupt = "format_version: 2\nmodules: {}\n_system:\n  network_peers: 损坏的记录\n";
    fs::write(&path, corrupt).unwrap();
    assert!(
        PairingService::new(fixture.stored_config())
            .list()
            .await
            .is_err()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), corrupt);
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
    service.start().await.unwrap();
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
    config
        .replace("network", "relay_token", json!("rejected-token"))
        .await
        .unwrap();
    assert!(service.start().await.is_err());
    assert_eq!(http(app.clone(), "GET", &path).await.1["status"], "pending");
    fixture
        .registrar
        .fail
        .store(false, std::sync::atomic::Ordering::SeqCst);
    config
        .replace("network", "relay_token", json!("new-token"))
        .await
        .unwrap();
    service.start().await.unwrap();
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
async fn services_mounts_network_apis_and_access_without_exposing_credentials() {
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
    assert_eq!(metadata["apis"].as_array().unwrap().len(), 6);
    for api in metadata["apis"].as_array().unwrap() {
        let expected = if api["id"] == "session" {
            "paired"
        } else {
            "local"
        };
        assert_eq!(api["access"], expected);
    }
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
    let notification = Arc::new(
        crate::modules::notification::NotificationService::with_launcher(
            fixture.launcher.clone(),
            Duration::from_secs(1),
        ),
    );
    let service = Arc::new(NetworkService::new(
        fixture.stored_config(),
        Arc::new(IrohRelayRegistrar::new()),
        notification.clone(),
        Arc::new(crate::modules::alert::AlertService::new(
            fixture.status.clone(),
            notification.clone(),
        )),
        fixture.status.clone(),
        Duration::from_secs(10),
    ));
    let config = fixture.config(service.clone());
    let remote_fixture = Fixture::new();
    let remote_network = remote_fixture.service();
    let remote_config = crate::modules::config::Module::from_service(
        remote_network
            .config()
            .with_change_handler("network", remote_network),
    );
    let remote_config_service = remote_config.service();
    let routes = remote_config
        .router()
        .merge(crate::modules::discover::Module::new().router())
        .merge(fixture.status.router())
        .merge(crate::modules::password::Module::new(None).router())
        .merge(
            crate::modules::emacs::Module::new(
                remote_config_service.clone(),
                notification,
                fixture.status.clone(),
            )
            .router(),
        );
    let weak = Arc::downgrade(&service);
    service.set_remote_dispatcher(Arc::new(super::remote::HttpDispatcher::new(move || {
        weak.upgrade().map(|service| {
            service
                .router()
                .merge(routes.clone())
                .merge(crate::http::mount_routes(
                    axum::Router::new(),
                    "network-test",
                ))
        })
    })));
    config
        .replace("network", "relay_url", json!(address))
        .await
        .unwrap();
    config
        .replace("network", "relay_token", json!(token))
        .await
        .unwrap();
    service.start().await.unwrap();
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
    let exchange_alpn = |body: Vec<u8>, alpn: &'static [u8]| {
        let endpoint = endpoint.clone();
        let remote = remote.clone();
        async move {
            let connection = endpoint.connect(remote, alpn).await.unwrap();
            let (mut send, mut recv) = connection.open_bi().await.unwrap();
            send.write_all(&body).await.unwrap();
            send.finish().unwrap();
            let data = recv.read_to_end(16_384).await.unwrap();
            serde_json::from_slice::<Value>(&data).unwrap()
        }
    };
    let exchange = |body| exchange_alpn(body, PAIRING_ALPN);
    let status_request =
        serde_json::to_vec(&json!({ "api_version": "1", "type": "device.status" })).unwrap();
    let unbound = exchange_alpn(status_request.clone(), super::protocol::DEVICE_STATUS_ALPN).await;
    assert_eq!(unbound["error"]["code"], "peer-not-bound");
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
    let response = tokio::time::timeout(Duration::from_secs(10), exchange(good.clone()))
        .await
        .unwrap();
    assert_eq!(response["type"], "pairing.bound");
    let repeated = exchange(good).await;
    assert_eq!(repeated["status"], 410);
    assert_eq!(repeated["error"]["code"], "pairing-expired");
    assert_eq!(
        exchange_alpn(status_request.clone(), super::protocol::DEVICE_STATUS_ALPN).await["error"]["code"],
        "invalid-binding-token"
    );
    let mut authenticated_status: Value = serde_json::from_slice(&status_request).unwrap();
    authenticated_status["binding_token"] = response["binding_token"].clone();
    let online = exchange_alpn(
        serde_json::to_vec(&authenticated_status).unwrap(),
        super::protocol::DEVICE_STATUS_ALPN,
    )
    .await;
    assert_eq!(online["status"], "online");
    assert_eq!(
        service.peers().await.unwrap()[0].device_type,
        super::protocol::DeviceType::Mobile
    );
    assert!(
        !serde_json::to_string(&service.peers().await.unwrap())
            .unwrap()
            .contains(response["binding_token"].as_str().unwrap())
    );
    let mut raw = json!({"api_version":"1","request_id":uuid::Uuid::new_v4().to_string(),"method":"GET","path":"/network/session","query":[]});
    assert_eq!(
        exchange_alpn(
            serde_json::to_vec(&raw).unwrap(),
            super::protocol::SERVICES_ALPN
        )
        .await["body"]["error"]["code"],
        "invalid-binding-token"
    );
    raw["binding_token"] = json!("0".repeat(64));
    assert_eq!(
        exchange_alpn(
            serde_json::to_vec(&raw).unwrap(),
            super::protocol::SERVICES_ALPN
        )
        .await["body"]["error"]["code"],
        "invalid-binding-token"
    );
    raw["binding_token"] = response["binding_token"].clone();
    let valid = exchange_alpn(
        serde_json::to_vec(&raw).unwrap(),
        super::protocol::SERVICES_ALPN,
    )
    .await;
    assert_eq!(valid["status"], 200);
    assert_eq!(valid["body"]["device_type"], "computer");
    assert!(!valid["body"]["name"].as_str().unwrap().is_empty());
    assert_eq!(online["endpoint_id"], created.qr_payload.endpoint_id);
    let spoofed_status = serde_json::to_vec(&json!({
        "api_version": "1", "type": "device.status", "endpoint_id": phone_id.to_string(),
    }))
    .unwrap();
    assert_eq!(
        exchange_alpn(spoofed_status, super::protocol::DEVICE_STATUS_ALPN).await["status"],
        400
    );
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
    assert_eq!(
        exchange_alpn(status_request, super::protocol::DEVICE_STATUS_ALPN).await["error"]["code"],
        "peer-not-bound"
    );

    // 业务保存 Network 返回的完整配置，重建 Workspace 后显式设置恢复。
    let mobile_path = fixture.root.join("mobile");
    fs::create_dir(&mobile_path).unwrap();
    let directory = mobile_path.clone();
    let workspace = Arc::new(
        tokio::task::spawn_blocking(move || {
            loki_mobile_network::Workspace::open(loki_mobile_network::WorkspaceOptions::new(
                directory,
            ))
            .unwrap()
        })
        .await
        .unwrap(),
    );
    let invitation = service.create_pairing().await.unwrap();
    use loki_mobile_network::network::{
        BindingState, QueryParameter, ResponsError, ServiceRequest,
    };
    let config = json!({"endpoint_id":invitation.qr_payload.endpoint_id,"relay_url":invitation.qr_payload.relay_url,"relay_token":invitation.qr_payload.relay_token,"binding":{"status":"unbound"},"auto_binding":{"invitation":invitation.qr_payload,"device_name":"测试手机"}});
    let mut wrong = config.clone();
    wrong["auto_binding"]["invitation"]["pairing_token"] = json!("0".repeat(64));
    assert_eq!(
        mobile_execute(workspace.clone(), "set_service", wrong.to_string()).await["ok"],
        true
    );
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let info=workspace.network().service().unwrap();
            if let BindingState::Failed{error}=info.binding {
                assert!(matches!(error,ResponsError::Services(error) if error.code == "invalid-pairing-token"));
                assert!(info.auto_binding.is_none());
                break;
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert!(matches!(
        channel_request(workspace.network(), ServiceRequest::get("/network/session"))
            .await
            .unwrap_err(),
        ResponsError::NotBound(_)
    ));
    assert_eq!(
        mobile_execute(workspace.clone(), "set_service", config.to_string()).await["ok"],
        true
    );
    let info = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = serde_json::to_value(workspace.network().service().unwrap()).unwrap();
            if info["binding"]["status"] == "bound" {
                break info;
            }
            assert_ne!(info["binding"]["status"], "failed", "自动绑定失败");
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(info.get("auto_binding").is_none());
    let token = info["binding"]["credential"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        mobile_execute(workspace.clone(), "bind", String::new()).await["ok"],
        false
    );
    let config = json!({"endpoint_id":info["endpoint_id"],"relay_url":info["relay_url"],"relay_token":info["relay_token"],"binding":info["binding"]});
    assert_eq!(
        mobile_execute(
            workspace.clone(),
            "save_state",
            json!({"service":config}).to_string()
        )
        .await["ok"],
        true
    );
    let phone_peer = service
        .peers()
        .await
        .unwrap()
        .into_iter()
        .find(|peer| peer.name == "测试手机")
        .unwrap();
    assert_eq!(phone_peer.device_type, super::protocol::DeviceType::Mobile);
    let mobile_id = phone_peer.endpoint_id;
    mobile_execute(workspace.clone(), "stop", String::new()).await;
    drop(workspace);
    let directory = mobile_path.clone();
    let workspace = Arc::new(
        tokio::task::spawn_blocking(move || {
            loki_mobile_network::Workspace::open(loki_mobile_network::WorkspaceOptions::new(
                directory,
            ))
            .unwrap()
        })
        .await
        .unwrap(),
    );
    assert!(!workspace.network().has_service());
    let saved = mobile_execute(workspace.clone(), "load_state", String::new()).await;
    assert_eq!(
        mobile_execute(
            workspace.clone(),
            "set_service",
            saved["state"]["service"].to_string()
        )
        .await["ok"],
        true
    );
    let network = workspace.network();
    // 同一身份与绑定凭据重新连接，无需新邀请；完整配置由应用保存。
    let restored = channel_request(network.clone(), ServiceRequest::get("/network/session"))
        .await
        .unwrap();
    assert_eq!(restored.data["status"], "online");
    assert_eq!(restored.data["device_type"], "computer");
    assert!(!restored.data["name"].as_str().unwrap().is_empty());
    assert!(!restored.data.to_string().contains(&token));
    let debug_address = workspace.start_http_server(0, |_, _, _| false).unwrap();
    let (status, body) = mobile_http(debug_address, "/service/network/session").await;
    assert_eq!(status, 200);
    assert_eq!(body, restored.data);
    let (status, body) = mobile_http(
        debug_address,
        "/service/network-test/echo/phone%20test?text=%E4%B8%AD+%26+%2B&text=second",
    )
    .await;
    assert_eq!(status, 202);
    assert_eq!(body["id"], "phone test");
    let query = body["query"].as_str().unwrap();
    assert_eq!(
        url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>(),
        [
            ("text".into(), "中 & +".into()),
            ("text".into(), "second".into())
        ]
    );
    let (status, body) = mobile_http(debug_address, "/service/network/peers").await;
    assert_eq!(status, 403);
    assert_eq!(body["code"], "remote-api-forbidden");
    let (status, body) = mobile_http(debug_address, "/service/missing").await;
    assert_eq!(status, 404);
    assert_eq!(body["status"], 404);
    assert_eq!(
        mobile_execute(workspace.clone(), "stop_http", String::new()).await["ok"],
        true
    );
    assert!(tokio::net::TcpStream::connect(debug_address).await.is_err());
    let mut request = ServiceRequest::get("/network-test/echo/phone%20test");
    request.method = axum::http::Method::POST;
    request.query = vec![
        QueryParameter {
            name: "text".into(),
            value: "中文 & +".into(),
        },
        QueryParameter {
            name: "text".into(),
            value: "second".into(),
        },
    ];
    request.body = Some(json!({"value":"手机请求"}));
    let echoed = channel_request(network.clone(), request.clone())
        .await
        .unwrap();
    assert_eq!(echoed.status, 202);
    assert_eq!(echoed.data["id"], "phone test");
    assert_eq!(echoed.data["device"], mobile_id);
    assert_eq!(echoed.data["remote_device"], mobile_id);
    let query = echoed.data["query"].as_str().unwrap();
    assert_eq!(
        url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>()[0].1,
        "中文 & +"
    );
    assert_eq!(
        serde_json::from_str::<Value>(echoed.data["body"].as_str().unwrap()).unwrap(),
        json!({"value":"手机请求"})
    );
    request.body = None;
    assert_eq!(
        channel_request(network.clone(), request.clone())
            .await
            .unwrap()
            .data["body"],
        ""
    );
    request.body = Some(Value::Null);
    assert_eq!(
        channel_request(network.clone(), request)
            .await
            .unwrap()
            .data["body"],
        "null"
    );
    let mut calls = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let network = network.clone();
        calls.spawn(async move {
            channel_request(network, ServiceRequest::get("/network/session"))
                .await
                .unwrap()
        });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(response) = calls.join_next().await {
        let response = response.unwrap();
        assert_eq!(response.data["status"], "online");
        assert!(ids.insert(response.request_id));
    }
    for (method, path) in [
        ("GET", "/password/entries"),
        ("GET", "/password/entries/test/item.gpg"),
        ("GET", "/network-test/echo/local"),
        ("POST", "/network/pairings"),
        ("DELETE", "/network/peers/test"),
        ("GET", "/emacs/buffers"),
        ("POST", "/emacs/eval"),
    ] {
        let mut request = ServiceRequest::get(path);
        request.method = method.parse().unwrap();
        let forbidden = channel_request(network.clone(), request).await.unwrap_err();
        assert!(
            matches!(forbidden, ResponsError::Services(error) if error.status == 403 && error.code == "remote-api-forbidden"),
            "{method} {path}"
        );
    }
    check_remote_metadata_and_permissions(network.clone(), &fixture, &remote_config_service).await;
    let missing = channel_request(network.clone(), ServiceRequest::get("/missing"))
        .await
        .unwrap_err();
    assert!(matches!(missing,ResponsError::Services(error) if error.status == 404));
    let mut wrong_method = ServiceRequest::get("/network/session");
    wrong_method.method = axum::http::Method::POST;
    let error = channel_request(network.clone(), wrong_method)
        .await
        .unwrap_err();
    assert!(matches!(error,ResponsError::Services(error) if error.status == 405));
    let error = channel_request(network.clone(), ServiceRequest::get("/network-test/bad"))
        .await
        .unwrap_err();
    assert!(
        matches!(error,ResponsError::Services(error) if error.code == "invalid-service-response")
    );
    let error = channel_request(network.clone(), ServiceRequest::get("/network-test/large"))
        .await
        .unwrap_err();
    assert!(matches!(error,ResponsError::Services(error) if error.code == "response-too-large"));
    assert_eq!(
        http(service.router(), "GET", "/network/session").await.0,
        401
    );
    // 在保持连接的情况下解绑，后续请求不得继续进入业务路由。
    service
        .remove_peer(mobile_id.parse().unwrap())
        .await
        .unwrap();
    let denied = channel_request(network.clone(), ServiceRequest::get("/network/session"))
        .await
        .unwrap_err();
    assert!(matches!(
        denied,
        ResponsError::NotBound(_) | ResponsError::Network(_)
    ));
    // 新连接仍携带旧凭据，电脑必须按身份拒绝；Network 清除无效绑定数据。
    let denied = channel_request(network.clone(), ServiceRequest::get("/network/session"))
        .await
        .unwrap_err();
    assert!(matches!(denied, ResponsError::NotBound(_)));
    assert!(matches!(
        network.service().unwrap().binding,
        BindingState::Failed { .. }
    ));
    workspace.close().await;
    service.shutdown().await;
    phone.close().await.unwrap();
    relay.shutdown().await.unwrap();
}

async fn check_remote_metadata_and_permissions(
    network: Arc<loki_mobile_network::network::Network>,
    fixture: &Fixture,
    config: &crate::modules::config::service::ConfigService,
) {
    use loki_mobile_network::network::{ResponsError, ServiceRequest};

    config
        .replace("network", "relay_token", json!("本机配置"))
        .await
        .unwrap();
    fixture
        .status
        .publish("emacs", "runtime", json!({"pid":123}))
        .await
        .unwrap();
    fixture
        .status
        .publish("custom", "private", json!(true))
        .await
        .unwrap();
    fixture
        .status
        .publish("slack", "connections", json!([{"session_id":"本机会话"}]))
        .await
        .unwrap();
    fixture
        .status
        .publish("service", "name", json!("测试电脑"))
        .await
        .unwrap();
    fixture
        .status
        .publish("mail", "unread_count", json!(3))
        .await
        .unwrap();
    let get = |path: &'static str| channel_request(network.clone(), ServiceRequest::get(path));
    let catalog = get("/discover").await.unwrap().data;
    let modules = catalog["modules"].as_array().unwrap();
    for module in modules {
        let apis = module["apis"].as_array().unwrap();
        assert!(!apis.is_empty());
        assert!(apis.iter().all(|api| api["access"] == "paired"));
    }
    let summaries = get("/discover/modules").await.unwrap().data;
    let summaries = summaries["modules"].as_array().unwrap();
    assert!(
        summaries
            .iter()
            .map(|module| &module["id"])
            .eq(modules.iter().map(|module| &module["id"]))
    );
    let doc = get("/discover/modules/doc").await.unwrap().data;
    let apis = doc["apis"].as_array().unwrap();
    assert_eq!(apis.len(), 1);
    assert_eq!(apis[0]["method"], "GET");
    assert_eq!(apis[0]["path"], "/doc/read-later/entries");
    let error = get("/discover/modules/password").await.unwrap_err();
    assert!(
        matches!(error, ResponsError::Services(error) if error.status == 404 && error.code == "module-not-found")
    );
    assert_eq!(
        get("/data/network").await.unwrap().data["values"],
        json!({"relay_url":{"value":"","source":"default"}})
    );
    let metadata = get("/meta/network").await.unwrap().data;
    assert_eq!(metadata["items"].as_array().unwrap().len(), 1);
    assert_eq!(metadata["items"][0]["key"], "relay_url");
    assert_eq!(metadata["items"][0]["remote_access"], "read");
    assert_eq!(
        get("/data/control-center/center_auto_start")
            .await
            .unwrap()
            .data["value"],
        false
    );
    assert_eq!(
        get("/status/service/name").await.unwrap().data["value"],
        "测试电脑"
    );
    assert_eq!(
        get("/status/network/my/status").await.unwrap().data["value"],
        "connected"
    );
    assert_eq!(
        get("/status/mail/unread_count").await.unwrap().data["value"],
        3
    );
    let snapshot = get("/status").await.unwrap().data;
    assert_eq!(snapshot["status"]["mail"], json!({"unread_count": 3}));
    assert!(snapshot["status"].get("emacs").is_none());
    assert!(snapshot["status"].get("custom").is_none());
    assert!(snapshot["status"].get("slack").is_none());
    assert!(
        get("/stats/meta").await.unwrap().data["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|module| !matches!(module["id"].as_str(), Some("emacs" | "slack")))
    );

    for (method, path, body, code) in [
        (
            "GET",
            "/data/network/relay_token",
            None,
            "remote-config-forbidden",
        ),
        (
            "PUT",
            "/editing/network/relay_url",
            Some(json!("其他地址")),
            "remote-config-forbidden",
        ),
        (
            "POST",
            "/editing/network/relay_token",
            Some(json!("其他令牌")),
            "remote-config-forbidden",
        ),
        (
            "PUT",
            "/editing/control-center/center_auto_start",
            Some(json!(true)),
            "remote-config-forbidden",
        ),
        (
            "PATCH",
            "/editing/control-center/center_auto_start",
            Some(json!(true)),
            "remote-config-forbidden",
        ),
        (
            "POST",
            "/config/validate",
            Some(json!({"module_id":"network","key":"relay_token","value":"其他令牌"})),
            "remote-config-forbidden",
        ),
        (
            "GET",
            "/status/emacs/runtime",
            None,
            "remote-status-forbidden",
        ),
        (
            "GET",
            "/status/emacs/runtime/pid",
            None,
            "remote-status-forbidden",
        ),
        (
            "GET",
            "/status/custom/private",
            None,
            "remote-status-forbidden",
        ),
        (
            "GET",
            "/status/slack/connections/0/session_id",
            None,
            "remote-status-forbidden",
        ),
        (
            "POST",
            "/emacs/receiver/status",
            Some(json!({"service":{"name":"禁止提交的名称"}})),
            "remote-api-forbidden",
        ),
    ] {
        let mut request = ServiceRequest::get(path);
        request.method = method.parse().unwrap();
        request.body = body;
        let error = channel_request(network.clone(), request).await.unwrap_err();
        assert!(
            matches!(error, ResponsError::Services(error) if error.status == 403 && error.code == code),
            "{method} {path}"
        );
    }
    assert_eq!(
        fixture.status.value("service", "name").await.unwrap().value,
        "测试电脑"
    );

    assert_eq!(
        config
            .value("control-center", "center_auto_start")
            .await
            .unwrap()
            .value,
        false
    );
    assert_eq!(
        config.value("network", "relay_token").await.unwrap().value,
        true
    );
    assert_eq!(
        fixture
            .status
            .value("emacs", "runtime")
            .await
            .unwrap()
            .value["pid"],
        123
    );
}

#[loki_services_macros::loki_api(
    module = "network-test",
    mount = "()",
    id = "echo",
    method = "POST",
    path = "/network-test/echo/{id}",
    access = "paired",
    summary = "测试通用 HTTP 通道",
    description = "仅测试时编译，验证路径、查询与请求体透传。"
)]
async fn echo_channel(
    axum::extract::Path(id): axum::extract::Path<String>,
    query: axum::extract::RawQuery,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::ACCEPTED,
        axum::Json(
            json!({"id":id,"query":query.0,"body":String::from_utf8(body.to_vec()).unwrap(),
                "device":headers.get("x-loki-device").and_then(|value| value.to_str().ok()),
                "remote_device":headers.get("x-loki-remote-devices").and_then(|value| value.to_str().ok())}),
        ),
    )
}

#[loki_services_macros::loki_api(
    module = "network-test",
    mount = "()",
    id = "query",
    method = "GET",
    path = "/network-test/echo/{id}",
    access = "paired",
    summary = "测试手机 HTTP 查询",
    description = "仅测试时编译，验证路径和重复查询参数。"
)]
async fn query_channel(
    id: axum::extract::Path<String>,
    query: axum::extract::RawQuery,
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    echo_channel(id, query, headers, axum::body::Bytes::new()).await
}

#[loki_services_macros::loki_api(
    module = "network-test",
    mount = "()",
    id = "local",
    method = "GET",
    path = "/network-test/echo/local",
    summary = "测试本机路由",
    description = "验证固定本机路径优先于带参数的远端路由。"
)]
async fn local_channel() -> &'static str {
    "本机"
}

#[loki_services_macros::loki_api(
    module = "network-test",
    mount = "()",
    id = "bad",
    method = "GET",
    path = "/network-test/bad",
    access = "paired",
    summary = "测试无效响应",
    description = "仅测试时编译。"
)]
async fn bad_channel() -> &'static str {
    "not json"
}

#[loki_services_macros::loki_api(
    module = "network-test",
    mount = "()",
    id = "large",
    method = "GET",
    path = "/network-test/large",
    access = "paired",
    summary = "测试超限响应",
    description = "仅测试时编译。"
)]
async fn large_channel() -> axum::Json<Value> {
    axum::Json(json!({"data":"x".repeat(4_200_000)}))
}

async fn channel_request(
    network: Arc<loki_mobile_network::network::Network>,
    request: loki_mobile_network::network::ServiceRequest,
) -> Result<
    loki_mobile_network::network::ServiceResponse<Value>,
    loki_mobile_network::network::ResponsError,
> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(sender)));
    let error_sender = sender.clone();
    network.request(
        request,
        Default::default(),
        move |response| {
            if let Some(sender) = sender.lock().unwrap().take() {
                let _ = sender.send(Ok(response));
            }
        },
        move |error| {
            if let Some(sender) = error_sender.lock().unwrap().take() {
                let _ = sender.send(Err(error));
            }
        },
    );
    receiver.await.unwrap()
}

async fn mobile_execute(
    workspace: Arc<loki_mobile_network::Workspace>,
    operation: &'static str,
    payload: String,
) -> Value {
    tokio::task::spawn_blocking(move || workspace.execute(operation, &payload))
        .await
        .unwrap()
}

async fn mobile_http(address: std::net::SocketAddr, path: &str) -> (u16, Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut connection = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    connection.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).await.unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    let status = headers
        .split_ascii_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, serde_json::from_str(body).unwrap())
}

#[tokio::test]
async fn binding_credentials_are_persistent_peer_scoped_and_revoked_on_rebinding() {
    let fixture = Fixture::new();
    let manager = Arc::new(PairingService::new(fixture.stored_config()));
    let server = SecretKey::generate().public().to_string();
    let phone = SecretKey::generate().public();
    let other = SecretKey::generate().public();
    let invitation = manager
        .create(
            server.clone(),
            "https://relay.example/".into(),
            "relay".into(),
        )
        .await
        .unwrap();
    let bound = manager
        .bind(
            phone,
            request(&invitation.pairing_id, &invitation.qr_payload.pairing_token),
        )
        .await
        .unwrap();
    let token = bound.binding_token.0.clone();
    assert_eq!(token.len(), 64);
    manager.authenticate(phone, &token).await.unwrap();
    assert_eq!(
        manager
            .authenticate(other, &token)
            .await
            .unwrap_err()
            .code(),
        "peer-not-bound"
    );
    assert_eq!(
        manager.authenticate(phone, "").await.unwrap_err().code(),
        "invalid-binding-token"
    );
    assert_eq!(
        manager
            .authenticate(phone, &"0".repeat(64))
            .await
            .unwrap_err()
            .code(),
        "invalid-binding-token"
    );
    assert!(!format!("{bound:?}").contains(&token));
    assert!(
        !serde_json::to_string(&manager.list().await.unwrap())
            .unwrap()
            .contains(&token)
    );
    let restarted = PairingService::new(fixture.stored_config());
    restarted.authenticate(phone, &token).await.unwrap();
    let next = manager
        .create(server, "https://relay.example/".into(), "relay".into())
        .await
        .unwrap();
    let rebound = manager
        .bind(
            phone,
            request(&next.pairing_id, &next.qr_payload.pairing_token),
        )
        .await
        .unwrap();
    assert_ne!(rebound.binding_token.0, token);
    assert_eq!(
        manager
            .authenticate(phone, &token)
            .await
            .unwrap_err()
            .code(),
        "invalid-binding-token"
    );
    manager
        .authenticate(phone, &rebound.binding_token.0)
        .await
        .unwrap();
    assert_eq!(
        manager
            .bind(
                phone,
                request(&invitation.pairing_id, &invitation.qr_payload.pairing_token)
            )
            .await
            .unwrap_err()
            .code(),
        "pairing-expired"
    );
    manager.remove(phone).await.unwrap();
    assert_eq!(
        manager
            .authenticate(phone, &rebound.binding_token.0)
            .await
            .unwrap_err()
            .code(),
        "peer-not-bound"
    );
}
