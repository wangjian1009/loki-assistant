use super::*;
use crate::network::*;
use crate::storage::*;
use iroh::SecretKey;
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{fs, sync::Arc, time::Duration};

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("loki-mobile-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, operation: &str, payload: &str) -> Value {
        match Workspace::open(WorkspaceOptions::new(&self.0)) {
            Ok(workspace) => workspace.execute(operation, payload),
            Err(error) => json!({"ok":false,"error":error}),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invitation() -> Value {
    json!({ "version": 1, "pairing_id": uuid::Uuid::new_v4().to_string(),
        "endpoint_id": SecretKey::generate().public().to_string(),
        "relay_url": "https://relay.example/", "relay_token": "relay-secret",
        "alpn": "loki-assistant/pair/1", "pairing_token": "a".repeat(64), "expires_at": now() + 300 })
}

#[test]
fn rejects_invalid_expired_and_foreign_qr_codes() {
    assert!(parse_invitation("https://example.org").is_err());
    let valid = invitation();
    assert!(parse_invitation(&valid.to_string()).is_ok());
    for (key, value) in [
        ("version", json!(2)),
        ("alpn", json!("other/1")),
        ("endpoint_id", json!("invalid")),
        ("pairing_id", json!("invalid")),
        ("pairing_token", json!("A".repeat(64))),
        ("expires_at", json!(now() - 1)),
        ("relay_url", json!("https://user:password@relay.example/")),
        ("relay_url", json!("file:///tmp/relay")),
        ("relay_token", json!(" ")),
    ] {
        let mut bad = valid.clone();
        bad[key] = value;
        assert!(parse_invitation(&bad.to_string()).is_err(), "{key}");
    }
}

#[test]
fn restores_identity_and_records_without_exposing_credentials_or_stale_online_state() {
    let fixture = Fixture::new();
    assert_eq!(fixture.run("list", "")["devices"], json!([]));
    let path = fixture.0.join("devices.json");
    let mut store = load(&path).unwrap();
    let id = store.endpoint_id.clone();
    let qr: Invitation = serde_json::from_value(invitation()).unwrap();
    store.computers.push(Computer {
        endpoint_id: qr.endpoint_id,
        relay_url: qr.relay_url,
        relay_token: qr.relay_token,
        bound_at: Some(now()),
        pending: None,
    });
    save(&path, &store).unwrap();
    let restored = fixture.run("list", "");
    assert_eq!(restored["devices"][0]["status"], "unknown");
    assert_eq!(load(&path).unwrap().endpoint_id, id);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let view = restored.to_string();
    assert!(!view.contains("relay-secret"));
    assert!(!view.contains(&store.secret_key));
}

#[test]
fn damaged_or_symlinked_storage_is_preserved() {
    let fixture = Fixture::new();
    let path = fixture.0.join("devices.json");
    fs::write(&path, "broken").unwrap();
    assert_eq!(fixture.run("list", "")["ok"], false);
    assert_eq!(fs::read_to_string(&path).unwrap(), "broken");
    fs::remove_file(&path).unwrap();
    let target = fixture.0.join("target.json");
    fs::write(&target, "preserve").unwrap();
    symlink(&target, &path).unwrap();
    assert_eq!(fixture.run("list", "")["ok"], false);
    assert_eq!(fs::read_to_string(&target).unwrap(), "preserve");
}

#[test]
fn workspace_injects_one_network_and_reports_missing_service_through_callback() {
    let fixture = Fixture::new();
    let workspace = Workspace::open(WorkspaceOptions::new(&fixture.0)).unwrap();
    let network = workspace.network();
    assert!(Arc::ptr_eq(&network, &workspace.network()));
    assert!(Arc::ptr_eq(&network, &workspace.devices().network()));
    let (sender, receiver) = std::sync::mpsc::channel();
    network.request::<Value, _, _>(
        ServiceRequest::get("/network/session"),
        RequestOptions::default(),
        |_| panic!("未配置 service 不得成功"),
        move |error| {
            sender.send(error).unwrap();
        },
    );
    let error = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(error.kind, ErrorKind::ServiceNotConfigured);
    assert_eq!(error.code, "service-not-configured");
    assert_eq!(error.outcome, Outcome::NotSent);
    assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
    assert_eq!(
        workspace.execute("request", r#"{"method":"GET","path":"/network/session"}"#)["network_error"]
            ["code"],
        "service-not-configured"
    );
}

#[test]
fn legacy_multiple_bindings_require_explicit_selection() {
    let fixture = Fixture::new();
    let path = fixture.0.join("devices.json");
    let mut store = load(&path).unwrap();
    for _ in 0..2 {
        let qr: Invitation = serde_json::from_value(invitation()).unwrap();
        store.computers.push(Computer {
            endpoint_id: qr.endpoint_id,
            relay_url: qr.relay_url,
            relay_token: qr.relay_token,
            bound_at: Some(now()),
            pending: None,
        });
    }
    save(&path, &store).unwrap();
    let workspace = Workspace::open(WorkspaceOptions::new(&fixture.0)).unwrap();
    assert!(!workspace.network().has_service());
    assert_eq!(
        workspace.execute("list", "")["devices"][0]["status"],
        "inactive"
    );
    let id = store.computers[1].endpoint_id.clone();
    assert_eq!(workspace.execute("select", &id)["ok"], true);
    assert!(workspace.network().has_service());
    assert_eq!(workspace.execute("list", "")["devices"][1]["active"], true);
    assert_eq!(load(&path).unwrap().selected_endpoint_id, Some(id));
    workspace.network().clear_service();
    assert!(!workspace.network().has_service());
}

#[test]
fn network_cancel_switch_timeout_and_close_complete_once() {
    let fixture = Fixture::new();
    let workspace = Workspace::open(WorkspaceOptions::new(&fixture.0)).unwrap();
    let network = workspace.network();
    let key = SecretKey::generate();
    let id = SecretKey::generate().public();
    let service = || {
        Service::new(
            key.clone(),
            id,
            "http://127.0.0.1:9/".into(),
            "test-token".into(),
        )
        .unwrap()
    };
    network.set_service(service()).unwrap();
    let generation = network.generation();
    network.set_service(service()).unwrap();
    assert_eq!(network.generation(), generation);
    let request = |network: &Network, timeout| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let handle = network.request::<Value, _, _>(
            ServiceRequest::get("/network/session"),
            RequestOptions { timeout },
            |_| panic!("不可达服务不得成功"),
            move |error| {
                sender.send(error).unwrap();
            },
        );
        (handle, receiver)
    };
    let (handle, receiver) = request(&network, Duration::from_secs(10));
    handle.cancel();
    handle.cancel();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap().kind,
        ErrorKind::Cancelled
    );
    assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
    let (_, receiver) = request(&network, Duration::from_secs(10));
    network.clear_service();
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap().kind,
        ErrorKind::ServiceChanged
    );
    network.set_service(service()).unwrap();
    let (_, receiver) = request(&network, Duration::from_secs(1));
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(3)).unwrap().kind,
        ErrorKind::Timeout
    );
    let (_, receiver) = request(&network, Duration::from_secs(10));
    workspace.execute("stop", "");
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap().kind,
        ErrorKind::Cancelled
    );
    let (_, receiver) = request(&network, Duration::from_secs(10));
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(2)).unwrap().kind,
        ErrorKind::Closed
    );
    assert!(network.set_service(service()).is_err());
}

#[test]
fn retained_network_delivers_closed_error_after_workspace_drop() {
    let fixture = Fixture::new();
    let workspace = Workspace::open(WorkspaceOptions::new(&fixture.0)).unwrap();
    let network = workspace.network();
    workspace.execute("close", "");
    drop(workspace);
    let (sender, receiver) = std::sync::mpsc::channel();
    network.request::<Value, _, _>(
        ServiceRequest::get("/network/session"),
        Default::default(),
        |_| panic!("已关闭不得成功"),
        move |error| {
            sender.send(error).unwrap();
        },
    );
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap().kind,
        ErrorKind::Closed
    );
}

#[test]
fn workspace_stop_cancels_all_callers_and_rejects_new_business_operations() {
    let fixture = Fixture::new();
    let workspace = Arc::new(Workspace::open(WorkspaceOptions::new(&fixture.0)).unwrap());
    let network = workspace.network();
    network
        .set_service(
            Service::new(
                SecretKey::generate(),
                SecretKey::generate().public(),
                "http://127.0.0.1:9/".into(),
                "token".into(),
            )
            .unwrap(),
        )
        .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    for _ in 0..4 {
        let sender = sender.clone();
        network.request::<Value, _, _>(
            ServiceRequest::get("/network/session"),
            Default::default(),
            |_| panic!("不可达服务不得成功"),
            move |error| {
                sender.send(error).unwrap();
            },
        );
    }
    drop(sender);
    let first = workspace.clone();
    let second = workspace.clone();
    let begin = std::time::Instant::now();
    let stop1 = std::thread::spawn(move || first.execute("stop", ""));
    let stop2 = std::thread::spawn(move || second.execute("stop", ""));
    assert_eq!(stop1.join().unwrap()["ok"], true);
    assert_eq!(stop2.join().unwrap()["ok"], true);
    assert!(begin.elapsed() < Duration::from_secs(4));
    let errors = receiver.into_iter().collect::<Vec<_>>();
    assert_eq!(errors.len(), 4);
    assert!(
        errors
            .iter()
            .all(|error| error.kind == ErrorKind::Cancelled)
    );
    assert_eq!(
        errors
            .iter()
            .filter_map(|e| e.request_id.clone())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4
    );
    assert_eq!(workspace.execute("list", "")["ok"], false);
    assert_eq!(workspace.execute("refresh", "")["ok"], false);
    assert_eq!(workspace.execute("select", "missing")["ok"], false);
    assert_eq!(workspace.execute("stop", "")["ok"], true);
}
