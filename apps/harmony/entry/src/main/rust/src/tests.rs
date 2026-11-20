use super::*;
use crate::{
    network::*,
    pairing::{Invitation, now},
    storage,
};
use iroh::SecretKey;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    sync::Arc,
    time::Duration,
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("loki-mobile-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn workspace(&self) -> Workspace {
        Workspace::open(WorkspaceOptions::new(&self.0)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn invitation() -> Value {
    json!({"version":1,"pairing_id":uuid::Uuid::new_v4().to_string(),
        "endpoint_id":SecretKey::generate().public().to_string(),"relay_url":"https://relay.example/",
        "relay_token":"relay-secret","alpn":"loki-assistant/pair/1","pairing_token":"a".repeat(64),"expires_at":now()+300})
}
fn bound() -> BindingState {
    serde_json::from_value(
        json!({"status":"bound","credential":{"token":"b".repeat(64),"bound_at":1}}),
    )
    .unwrap()
}
fn unreachable_service(key: &SecretKey, id: iroh::EndpointId) -> Service {
    Service::new(
        key.clone(),
        id,
        "http://127.0.0.1:9/".into(),
        "test-token".into(),
    )
    .unwrap()
    .with_binding(bound())
}
fn request(
    network: &Network,
    timeout: Duration,
) -> (RequestHandle, std::sync::mpsc::Receiver<ResponsError>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let handle = network.request::<Value, _, _>(
        ServiceRequest::get("/network/session"),
        RequestOptions { timeout },
        |_| panic!("请求不得成功"),
        move |error| {
            let _ = sender.send(error);
        },
    );
    (handle, receiver)
}
fn error_value(receiver: std::sync::mpsc::Receiver<ResponsError>) -> Value {
    serde_json::to_value(receiver.recv_timeout(Duration::from_secs(3)).unwrap()).unwrap()
}

#[test]
fn invitation_and_bound_state_require_protocol_data() {
    assert!(Invitation::parse("https://example.org").is_err());
    let valid = invitation();
    assert!(Invitation::parse(&valid.to_string()).is_ok());
    for (key, value) in [
        ("version", json!(2)),
        ("alpn", json!("other/1")),
        ("endpoint_id", json!("invalid")),
        ("pairing_id", json!("invalid")),
        ("pairing_token", json!("A".repeat(64))),
        ("expires_at", json!(now() - 1)),
        ("relay_url", json!("https://user:password@relay.example/")),
        ("relay_token", json!(" ")),
    ] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        assert!(Invitation::parse(&invalid.to_string()).is_err(), "{key}");
    }
    for invalid in [
        json!({"status":"bound"}),
        json!({"status":"bound","credential":{"token":"fake","bound_at":1}}),
        json!({"status":"unbound","credential":{"token":"a".repeat(64),"bound_at":1}}),
    ] {
        assert!(serde_json::from_value::<BindingState>(invalid).is_err());
    }
    assert!(!format!("{:?}", bound()).contains(&"b".repeat(64)));
}

#[test]
fn application_owns_records_and_workspace_never_selects_a_service() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let state = json!({"computers":[{"endpoint_id":"app-owned"}],"selected_endpoint_id":"app-owned","device_name":"手机"});
    assert_eq!(
        workspace.execute("save_state", &state.to_string())["ok"],
        true
    );
    let identity = workspace.execute("load_state", "")["endpoint_id"].clone();
    workspace.execute("stop", "");
    drop(workspace);
    let restored = fixture.workspace();
    let loaded = restored.execute("load_state", "");
    assert_eq!(loaded["state"], state);
    assert_eq!(loaded["endpoint_id"], identity);
    assert!(!restored.network().has_service());
    let secret = storage::load(&fixture.0.join("devices.json"))
        .unwrap()
        .secret_key;
    assert!(!loaded.to_string().contains(&secret));
    assert_eq!(
        restored.execute("save_state", r#"{"secret_key":"replace"}"#)["ok"],
        false
    );
    assert_eq!(
        storage::load(&fixture.0.join("devices.json"))
            .unwrap()
            .secret_key,
        secret
    );
    assert_eq!(
        fs::metadata(fixture.0.join("devices.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn damaged_or_symlinked_storage_is_preserved() {
    let fixture = Fixture::new();
    let path = fixture.0.join("devices.json");
    fs::write(&path, "broken").unwrap();
    assert!(Workspace::open(WorkspaceOptions::new(&fixture.0)).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
    fs::remove_file(&path).unwrap();
    let target = fixture.0.join("target.json");
    fs::write(&target, "preserve").unwrap();
    symlink(&target, &path).unwrap();
    assert!(Workspace::open(WorkspaceOptions::new(&fixture.0)).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "preserve");
}

#[test]
fn missing_unbound_invalid_and_transport_failures_are_distinct_unions() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let network = workspace.network();
    assert!(Arc::ptr_eq(&network, &workspace.network()));
    let (_, receiver) = request(&network, Duration::from_secs(10));
    let error = error_value(receiver);
    assert_eq!(error["kind"], "not_configured");
    assert_eq!(error["reason"], "service_not_set");
    let key = SecretKey::generate();
    let id = SecretKey::generate().public();
    network
        .set_service(
            Service::new(
                key.clone(),
                id,
                "http://127.0.0.1:9/".into(),
                "token".into(),
            )
            .unwrap(),
        )
        .unwrap();
    let (_, receiver) = request(&network, Duration::from_secs(10));
    let error = error_value(receiver);
    assert_eq!(error["kind"], "not_bound");
    assert_eq!(error["reason"], "binding_required");
    assert_eq!(error["outcome"], "not_sent");
    network.set_service(unreachable_service(&key, id)).unwrap();
    let (_, receiver) = request(&network, Duration::from_millis(1));
    let error = error_value(receiver);
    assert_eq!(error["kind"], "invalid_request");
    assert_eq!(error["reason"], "invalid_timeout");
    let (_, receiver) = request(&network, Duration::from_secs(1));
    let error = error_value(receiver);
    assert_eq!(error["kind"], "network");
    assert!(matches!(
        error["reason"].as_str(),
        Some("request_timeout" | "connect_failed")
    ));
}

#[test]
fn network_replaces_the_only_service_and_restores_full_configuration() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let network = workspace.network();
    let config = json!({"endpoint_id":SecretKey::generate().public().to_string(),"relay_url":"https://relay.example/","relay_token":"relay-secret","binding":bound()});
    assert_eq!(
        workspace.execute("set_service", &config.to_string())["ok"],
        true
    );
    let snapshot = workspace.execute("service", "")["service"].clone();
    for key in ["endpoint_id", "relay_url", "relay_token", "binding"] {
        assert_eq!(snapshot[key], config[key]);
    }
    let generation = network.generation();
    assert_eq!(
        workspace.execute("set_service", &config.to_string())["ok"],
        true
    );
    assert_eq!(network.generation(), generation);
    let mut other = config.clone();
    other["endpoint_id"] = json!(SecretKey::generate().public().to_string());
    assert_eq!(
        workspace.execute("set_service", &other.to_string())["ok"],
        true
    );
    assert_eq!(
        workspace.execute("service", "")["service"]["endpoint_id"],
        other["endpoint_id"]
    );
    network.clear_service();
    assert!(network.service().is_none());
}

#[test]
fn cancellation_switch_and_shutdown_preserve_separate_reasons() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let network = workspace.network();
    let key = SecretKey::generate();
    let id = SecretKey::generate().public();
    network.set_service(unreachable_service(&key, id)).unwrap();
    let (handle, receiver) = request(&network, Duration::from_secs(10));
    handle.cancel();
    handle.cancel();
    let error = error_value(receiver);
    assert_eq!(error["kind"], "cancelled");
    assert_eq!(error["reason"], "user_requested");
    let (_, receiver) = request(&network, Duration::from_secs(10));
    network.clear_service();
    assert_eq!(error_value(receiver)["reason"], "service_changed");
    network.set_service(unreachable_service(&key, id)).unwrap();
    let (_, receiver) = request(&network, Duration::from_secs(10));
    assert_eq!(workspace.execute("stop", "")["ok"], true);
    assert_eq!(error_value(receiver)["reason"], "network_closed");
    assert_eq!(workspace.execute("stop", "")["ok"], true);
    drop(workspace);
    let (_, receiver) = request(&network, Duration::from_secs(10));
    assert_eq!(error_value(receiver)["reason"], "network_closed");
}

#[test]
fn service_rejections_keep_their_own_reason_and_details() {
    let response = json!({"api_version":"1","status":409,"error":{"source":"business","code":"busy","message":"服务忙"}});
    let error = ResponsError::api(&response, 409).unwrap();
    let value = serde_json::to_value(error).unwrap();
    assert_eq!(value["kind"], "services");
    assert_eq!(value["reason"], "rejected");
    assert_eq!(value["status"], 409);
    assert_eq!(value["code"], "busy");
    assert_eq!(value["source"], "business");
    for (code, reason) in [
        ("peer-not-bound", "binding_revoked"),
        ("invalid-binding-token", "invalid_credential"),
    ] {
        let error=ResponsError::api(&json!({"api_version":"1","status":403,"error":{"source":"business","code":code,"message":"授权失败"}}),403).unwrap();
        let value = serde_json::to_value(error).unwrap();
        assert_eq!(value["kind"], "not_bound");
        assert_eq!(value["reason"], reason);
    }
    assert!(matches!(
        ResponsError::api(&response, 500).unwrap_err(),
        ResponsError::Network(_)
    ));
}

fn auto_service(key: &SecretKey, qr: &Value) -> Service {
    Service::new(
        key.clone(),
        qr["endpoint_id"].as_str().unwrap().parse().unwrap(),
        qr["relay_url"].as_str().unwrap().into(),
        qr["relay_token"].as_str().unwrap().into(),
    )
    .unwrap()
    .with_auto_binding(AutoBinding {
        invitation: serde_json::from_value(qr.clone()).unwrap(),
        device_name: "测试手机".into(),
    })
}
fn settled(network: &Network) -> ServiceState {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let service = network.service().unwrap();
        if !matches!(service.binding, BindingState::Binding) {
            return service;
        }
        assert!(std::time::Instant::now() < deadline, "自动绑定未及时完成");
        std::thread::yield_now();
    }
}

#[test]
fn setting_unbound_service_automatically_binds_and_retains_failure_for_ui() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let network = workspace.network();
    let key = SecretKey::generate();
    let mut qr = invitation();
    qr["expires_at"] = json!(now() - 1);
    network.set_service(auto_service(&key, &qr)).unwrap();
    let state = settled(&network);
    assert!(
        matches!(&state.binding, BindingState::Failed { error: ResponsError::InvalidRequest(failure) } if failure.reason == InvalidRequestReason::ExpiredInvitation)
    );
    assert!(state.auto_binding.is_none());
    let view = serde_json::to_value(&state).unwrap();
    assert_eq!(view["binding"]["error"]["reason"], "expired_invitation");
    assert!(
        !view
            .to_string()
            .contains(qr["pairing_token"].as_str().unwrap())
    );
    assert!(view["binding"]["error"]["request_id"].is_string());
    assert_eq!(workspace.execute("bind", "")["ok"], false);
    let (_, receiver) = request(&network, Duration::from_secs(1));
    assert_eq!(error_value(receiver)["kind"], "not_bound");
    assert_eq!(
        serde_json::to_value(network.service().unwrap().binding).unwrap(),
        view["binding"]
    );
    // 已记录的失败可序列化恢复；恢复失败不重新发送一次性邀请。
    network
        .set_service(
            Service::new(
                key,
                state.endpoint_id.parse().unwrap(),
                state.relay_url,
                state.relay_token,
            )
            .unwrap()
            .with_binding(state.binding),
        )
        .unwrap();
    assert!(matches!(
        network.service().unwrap().binding,
        BindingState::Failed { .. }
    ));
}

#[test]
fn binding_in_progress_is_single_and_cancelled_on_switch_or_shutdown() {
    let fixture = Fixture::new();
    let workspace = fixture.workspace();
    let network = workspace.network();
    let key = SecretKey::generate();
    let mut qr = invitation();
    qr["relay_url"] = json!("http://127.0.0.1:9/");
    network.set_service(auto_service(&key, &qr)).unwrap();
    assert!(matches!(
        network.service().unwrap().binding,
        BindingState::Binding
    ));
    let generation = network.generation();
    network.set_service(auto_service(&key, &qr)).unwrap();
    assert_eq!(network.generation(), generation);
    let (_, receiver) = request(&network, Duration::from_secs(1));
    assert_eq!(error_value(receiver)["kind"], "not_bound");
    assert!(matches!(
        network.service().unwrap().binding,
        BindingState::Binding
    ));
    let other = SecretKey::generate().public();
    network
        .set_service(unreachable_service(&key, other))
        .unwrap();
    assert_eq!(network.service().unwrap().endpoint_id, other.to_string());
    workspace.execute("stop", "");
    assert!(network.service().is_none());
}
