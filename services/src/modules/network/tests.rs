use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iroh::SecretKey;
use serde_json::{Value, json};

use super::{
    IrohRelayRegistrar, NetworkService, RelayRegistrar, RelaySession, identity,
    protocol::ConnectionStatus, relay::RelayFuture,
};
use crate::{
    modules::{
        config::{
            self,
            service::{ConfigService, ConfigServiceError},
        },
        notification::NotificationService,
        status::StatusService,
    },
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

#[derive(Debug, Default)]
pub(super) struct Launcher {
    messages: Mutex<Vec<NotificationContent>>,
    pub(super) fail: AtomicBool,
}

impl NotificationLauncher for Launcher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        Box::pin(async move {
            self.messages.lock().unwrap().push(content);
            if self.fail.load(Ordering::SeqCst) {
                Err("通知不可用".to_owned())
            } else {
                Ok(())
            }
        })
    }
}

#[derive(Debug, Default)]
struct Session(AtomicBool);
impl RelaySession for Session {
    fn status(&self) -> ConnectionStatus {
        if self.0.load(Ordering::SeqCst) {
            ConnectionStatus::Disconnected
        } else {
            ConnectionStatus::Connected
        }
    }
    fn close(&self) -> RelayFuture<'_, ()> {
        Box::pin(async {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct Registrar {
    calls: Mutex<Vec<(String, String, String)>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    pub(super) fail: AtomicBool,
    hang: AtomicBool,
    resume: tokio::sync::Notify,
    failure_message: Mutex<Option<String>>,
}
impl RelayRegistrar for Registrar {
    fn register(
        &self,
        identity: SecretKey,
        url: String,
        token: String,
    ) -> RelayFuture<'_, Arc<dyn RelaySession>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((url, identity.public().to_string(), token));
            if self.hang.load(Ordering::SeqCst) {
                self.resume.notified().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(self
                    .failure_message
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "中转拒绝注册".to_owned()));
            }
            let session = Arc::new(Session::default());
            self.sessions.lock().unwrap().push(session.clone());
            Ok(session as Arc<dyn RelaySession>)
        })
    }
}

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) registrar: Arc<Registrar>,
    pub(super) launcher: Arc<Launcher>,
    pub(super) status: Arc<StatusService>,
}
impl Fixture {
    pub(super) fn new() -> Self {
        let root = std::env::temp_dir().join(format!("loki-network-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("pass-store")).unwrap();
        let pass = root.join("test-pass");
        fs::write(
            &pass,
            r#"#!/bin/sh
set -eu
case "$1" in
    show) cat "$PASSWORD_STORE_DIR/$3.gpg" ;;
    insert)
        entry="$PASSWORD_STORE_DIR/$5.gpg"
        mkdir -p "$(dirname "$entry")"
        cat > "$entry"
        ;;
    rm) rm -f "$PASSWORD_STORE_DIR/$4.gpg" ;;
    *) exit 1 ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&pass, fs::Permissions::from_mode(0o700)).unwrap();
        let settings = json!({"password":{
            "store_dir":root.join("pass-store"), "pass_path":pass
        }});
        fs::write(
            root.join("pass-config.yml"),
            serde_yaml_ng::to_string(&settings).unwrap(),
        )
        .unwrap();
        Self {
            root,
            registrar: Arc::default(),
            launcher: Arc::default(),
            status: Arc::default(),
        }
    }
    pub(super) fn stored_config(&self) -> ConfigService {
        use crate::modules::password::{config::PasswordConfig, service::PasswordService};
        let settings = Arc::new(ConfigService::load(self.root.join("pass-config.yml")).unwrap());
        let passwords = Arc::new(PasswordService::new(PasswordConfig::new(settings)));
        ConfigService::load(self.root.join("config.yml"))
            .unwrap()
            .with_password_service(passwords)
    }
    pub(super) fn service(&self) -> Arc<NetworkService> {
        Arc::new(NetworkService::new(
            self.stored_config(),
            self.registrar.clone(),
            Arc::new(NotificationService::with_launcher(
                self.launcher.clone(),
                Duration::from_secs(1),
            )),
            self.status.clone(),
            Duration::from_millis(30),
        ))
    }
    pub(super) fn config(&self, service: Arc<NetworkService>) -> Arc<ConfigService> {
        Arc::new(service.config().with_change_handler("network", service))
    }
    fn phase(&self) -> Value {
        self.status.value("network", "my").unwrap().value["status"].clone()
    }
    fn titles(&self) -> Vec<String> {
        self.launcher
            .messages
            .lock()
            .unwrap()
            .iter()
            .map(|message| message.title.clone())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn creates_private_identity_atomically_and_reuses_it_across_instances() {
    let fixture = Fixture::new();
    let first_store = fixture.stored_config();
    let second_store = first_store.clone();
    let (first, second) = tokio::join!(
        identity::ensure(&first_store),
        identity::ensure(&second_store)
    );
    let (first, created_first) = first.unwrap();
    let (second, created_second) = second.unwrap();
    assert_ne!(created_first, created_second);
    assert_eq!(first.public(), second.public());
    let (loaded, created) = identity::ensure(&fixture.stored_config()).await.unwrap();
    assert!(!created);
    assert_eq!(loaded.public(), first.public());
    assert_eq!(
        fs::metadata(fixture.root.join("config.yml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let other = Fixture::new();
    assert_ne!(
        identity::ensure(&other.stored_config())
            .await
            .unwrap()
            .0
            .public(),
        first.public()
    );
}

#[tokio::test]
async fn corrupt_or_symlinked_identity_is_not_overwritten() {
    let fixture = Fixture::new();
    let path = fixture.root.join("identity.json");
    fs::write(&path, "损坏的身份").unwrap();
    assert!(identity::ensure(&fixture.stored_config()).await.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "损坏的身份");
    fs::remove_file(&path).unwrap();
    let target = fixture.root.join("target");
    fs::write(&target, "保留原文件").unwrap();
    symlink(&target, &path).unwrap();
    assert!(identity::ensure(&fixture.stored_config()).await.is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "保留原文件");
}

#[tokio::test]
async fn startup_without_relay_generates_identity_once_and_never_connects() {
    let fixture = Fixture::new();
    let service = fixture.service();
    service.start().await.unwrap();
    service.start().await.unwrap();
    assert_eq!(fixture.titles(), ["已生成本地身份"]);
    assert!(fixture.registrar.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn saving_relay_creates_identity_notifies_and_keeps_connection_until_replaced() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_token", json!("test-token"))
        .await
        .unwrap();
    config
        .replace("network", "relay_url", json!("https://first.example"))
        .await
        .unwrap();
    assert_eq!(fixture.titles(), ["已生成本地身份", "中转注册成功"]);
    config
        .replace("network", "relay_url", json!("https://first.example"))
        .await
        .unwrap();
    assert_eq!(fixture.registrar.calls.lock().unwrap().len(), 1);
    config
        .replace("network", "relay_url", json!("https://second.example"))
        .await
        .unwrap();
    let calls = fixture.registrar.calls.lock().unwrap().clone();
    assert_eq!(calls[0].1, calls[1].1);
    let sessions = fixture.registrar.sessions.lock().unwrap().clone();
    assert!(sessions[0].0.load(Ordering::SeqCst));
    assert!(!sessions[1].0.load(Ordering::SeqCst));
    config
        .replace("network", "relay_url", json!(""))
        .await
        .unwrap();
    assert!(sessions[1].0.load(Ordering::SeqCst));
    service.shutdown().await;
}

#[tokio::test]
async fn startup_registers_saved_relay_and_reports_failure_without_changing_config() {
    let fixture = Fixture::new();
    fs::write(
        fixture.root.join("config.yml"),
        "network:\n  relay_url: https://saved.example\n  relay_token: saved-token\n",
    )
    .unwrap();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    fixture.registrar.fail.store(true, Ordering::SeqCst);
    assert!(service.start().await.is_err());
    assert_eq!(fixture.titles(), ["已生成本地身份", "中转注册失败"]);
    assert_eq!(
        config.value("network", "relay_url").await.unwrap().value,
        json!("https://saved.example")
    );
    fixture.registrar.fail.store(false, Ordering::SeqCst);
    // 启动失败后，提交相同地址仍可重试。
    config
        .replace("network", "relay_url", json!("https://saved.example"))
        .await
        .unwrap();
    service.shutdown().await;
    let restarted = fixture.service();
    restarted.start().await.unwrap();
    assert_eq!(fixture.registrar.calls.lock().unwrap().len(), 3);
    assert_eq!(
        fixture
            .titles()
            .iter()
            .filter(|title| *title == "已生成本地身份")
            .count(),
        1
    );
    let calls = fixture.registrar.calls.lock().unwrap();
    assert_eq!(calls[0].1, calls[2].1);
    drop(calls);
    restarted.shutdown().await;
}

#[tokio::test]
async fn failed_change_returns_http_error_and_preserves_old_connection_and_file() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_token", json!("test-token"))
        .await
        .unwrap();
    config
        .replace("network", "relay_url", json!("https://old.example"))
        .await
        .unwrap();
    let saved = fs::read(fixture.root.join("config.yml")).unwrap();
    let mut changes = config.subscribe();
    fixture.registrar.fail.store(true, Ordering::SeqCst);
    let config = Arc::try_unwrap(config).unwrap();
    let module = config::Module::from_service(config);
    let response = module
        .router()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/data/network/relay_url")
                .header("content-type", "application/json")
                .body(Body::from("\"https://failed.example\""))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let body: serde_json::Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "config-change-rejected");
    assert_eq!(fs::read(fixture.root.join("config.yml")).unwrap(), saved);
    assert_eq!(
        module
            .service()
            .value("network", "relay_url")
            .await
            .unwrap()
            .value,
        json!("https://old.example")
    );
    assert!(
        !fixture.registrar.sessions.lock().unwrap()[0]
            .0
            .load(Ordering::SeqCst)
    );
    assert!(changes.try_recv().is_err());
    assert_eq!(fixture.titles().last().unwrap(), "中转注册失败");
    service.shutdown().await;
}

#[tokio::test]
async fn storage_failure_discards_candidate_without_success_notification() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    fs::write(&path, "network:\n  relay_token: test-token\n").unwrap();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    service.start().await.unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(matches!(
        config
            .replace("network", "relay_url", json!("https://new.example"))
            .await,
        Err(ConfigServiceError::Storage(_))
    ));
    assert_eq!(
        config.value("network", "relay_url").await.unwrap().value,
        json!("")
    );
    assert!(
        fixture.registrar.sessions.lock().unwrap()[0]
            .0
            .load(Ordering::SeqCst)
    );
    assert_eq!(fixture.titles(), ["已生成本地身份", "中转配置保存失败"]);
}

#[tokio::test]
async fn timeout_and_identity_failure_are_notified_and_notification_failure_does_not_change_registration_result()
 {
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_token", json!("test-token"))
        .await
        .unwrap();
    fixture.registrar.hang.store(true, Ordering::SeqCst);
    assert!(
        config
            .replace("network", "relay_url", json!("https://timeout.example"))
            .await
            .unwrap_err()
            .to_string()
            .contains("超时")
    );
    assert_eq!(fixture.titles().last().unwrap(), "中转注册失败");
    fixture.registrar.hang.store(false, Ordering::SeqCst);
    fixture.launcher.fail.store(true, Ordering::SeqCst);
    config
        .replace("network", "relay_url", json!("https://ok.example"))
        .await
        .unwrap();
    service.shutdown().await;
    config
        .set_system_value("network_identity", "损坏")
        .await
        .unwrap();
    assert!(service.start().await.is_err());
    assert_eq!(fixture.titles().last().unwrap(), "网络身份初始化失败");
}

#[tokio::test]
async fn real_iroh_registration_uses_only_the_configured_ephemeral_relay() {
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
                    reason: Some("令牌不匹配".to_owned()),
                }
            }
        }
    }
    let token = uuid::Uuid::new_v4().to_string();
    let mut server_config = ServerConfig::default();
    server_config.relay = Some(RelayConfig::new(
        "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
    ));
    server_config.relay.as_mut().unwrap().access = Arc::new(TokenAccess(token.clone()));
    let relay = Server::spawn(server_config).await.unwrap();
    let address = format!("http://{}", relay.http_addr().unwrap());
    let fixture = Fixture::new();
    let service = Arc::new(NetworkService::new(
        fixture.stored_config(),
        Arc::new(IrohRelayRegistrar::new()),
        Arc::new(NotificationService::with_launcher(
            fixture.launcher.clone(),
            Duration::from_secs(1),
        )),
        fixture.status.clone(),
        Duration::from_secs(10),
    ));
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_token", json!(token.clone()))
        .await
        .unwrap();
    config
        .replace("network", "relay_url", json!(address))
        .await
        .unwrap();
    assert_eq!(fixture.titles(), ["已生成本地身份", "中转注册成功"]);
    assert_eq!(fixture.phase(), "connected");
    let observer = service.clone();
    let monitor = tokio::spawn(async move { observer.monitor().await });
    let denied = IrohRelayRegistrar::new();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        denied.register(
            SecretKey::generate(),
            address,
            uuid::Uuid::new_v4().to_string(),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("认证"));
    assert!(error.contains("令牌不匹配"));
    relay.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.phase() != "failed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    service.shutdown().await;
    assert_eq!(fixture.phase(), "disconnected");
    monitor.abort();
    let _ = monitor.await;
}

#[tokio::test]
async fn invalid_relay_urls_are_rejected_without_fallback_connections() {
    let registrar = IrohRelayRegistrar::new();
    for url in [
        "not-a-url",
        "file:///tmp/relay",
        "ftp://relay.example",
        "http://user:password@127.0.0.1",
        "https://relay.example/?token=private",
    ] {
        assert!(
            registrar
                .register(
                    SecretKey::generate(),
                    url.to_owned(),
                    "test-token".to_owned()
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn incomplete_settings_are_saved_without_registration_on_startup_or_updates() {
    for (url, token) in [
        ("", ""),
        ("https://relay.example", ""),
        ("", "a-token"),
        (" ", "a-token"),
        ("not-a-url", " "),
    ] {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("config.yml"),
            serde_yaml_ng::to_string(&json!({"network":{"relay_url":url,"relay_token":token}}))
                .unwrap(),
        )
        .unwrap();
        let service = fixture.service();
        let config = fixture.config(service.clone());
        service.start().await.unwrap();
        config
            .replace("network", "relay_url", json!(url))
            .await
            .unwrap();
        config
            .replace("network", "relay_token", json!(token))
            .await
            .unwrap();
        assert!(fixture.registrar.calls.lock().unwrap().is_empty());
        assert_eq!(fixture.phase(), "offline");
        assert_eq!(fixture.titles(), ["已生成本地身份"]);
    }
}

#[tokio::test]
async fn either_setting_order_registers_only_after_both_are_saved_and_token_changes_reconnect() {
    for first in ["relay_url", "relay_token"] {
        let fixture = Fixture::new();
        let service = fixture.service();
        let config = fixture.config(service.clone());
        let second = if first == "relay_url" {
            "relay_token"
        } else {
            "relay_url"
        };
        for key in [first, second] {
            let value = if key == "relay_url" {
                "https://relay.example"
            } else {
                "first-token"
            };
            config.replace("network", key, json!(value)).await.unwrap();
            assert_eq!(
                fixture.registrar.calls.lock().unwrap().len(),
                if key == first { 0 } else { 1 }
            );
        }
        assert_eq!(fixture.phase(), "connected");
        assert_eq!(fixture.registrar.calls.lock().unwrap()[0].2, "first-token");
        config
            .replace("network", "relay_token", json!("rotated-token"))
            .await
            .unwrap();
        assert_eq!(fixture.registrar.calls.lock().unwrap().len(), 2);
        assert_eq!(
            fixture.registrar.calls.lock().unwrap()[1].2,
            "rotated-token"
        );
        assert!(
            fixture.registrar.sessions.lock().unwrap()[0]
                .0
                .load(Ordering::SeqCst)
        );
        let loaded = fixture.stored_config();
        assert_eq!(
            super::config::relay_token(&loaded).await.unwrap(),
            "rotated-token"
        );
        config
            .replace("network", "relay_token", json!(""))
            .await
            .unwrap();
        assert_eq!(fixture.phase(), "offline");
        assert_eq!(fixture.registrar.calls.lock().unwrap().len(), 2);
        assert!(
            fixture.registrar.sessions.lock().unwrap()[1]
                .0
                .load(Ordering::SeqCst)
        );
    }
}

#[tokio::test]
async fn status_read_during_registration_is_immediate_and_rejected_token_preserves_connection() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    let fixture = Fixture::new();
    let service = Arc::new(NetworkService::new(
        fixture.stored_config(),
        fixture.registrar.clone(),
        Arc::new(NotificationService::with_launcher(
            fixture.launcher.clone(),
            Duration::from_secs(1),
        )),
        fixture.status.clone(),
        Duration::from_secs(2),
    ));
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_token", json!("valid-token"))
        .await
        .unwrap();
    fixture.registrar.hang.store(true, Ordering::SeqCst);
    let saving = config.clone();
    let task = tokio::spawn(async move {
        saving
            .replace("network", "relay_url", json!("https://relay.example"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while fixture.registrar.calls.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let peer = super::peers::StoredPeer {
        device_type: super::protocol::DeviceType::default(),
        endpoint_id: SecretKey::generate().public().to_string(),
        name: "测试设备".into(),
        bound_at: 1,
        binding_token: None,
    };
    let endpoint_id = peer.endpoint_id.clone();
    super::peers::save(&config, vec![peer]).await.unwrap();
    let response = tokio::time::timeout(
        Duration::from_millis(100),
        fixture.status.router().oneshot(
            Request::builder()
                .uri("/status/network/my")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["value"]["status"], "connecting");
    assert!(!body.to_string().contains("valid-token"));
    fixture.registrar.hang.store(false, Ordering::SeqCst);
    fixture.registrar.resume.notify_one();
    task.await.unwrap().unwrap();
    assert!(
        super::peers::load(&fixture.stored_config())
            .await
            .unwrap()
            .contains_key(&endpoint_id)
    );
    assert_eq!(fixture.phase(), "connected");
    fixture.registrar.fail.store(true, Ordering::SeqCst);
    *fixture.registrar.failure_message.lock().unwrap() = Some("拒绝 secret-token".to_owned());
    let error = config
        .replace("network", "relay_token", json!("secret-token"))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("secret-token"));
    assert_eq!(
        super::config::relay_token(&config).await.unwrap(),
        "valid-token"
    );
    assert_eq!(fixture.phase(), "connected");
    assert!(
        fixture
            .launcher
            .messages
            .lock()
            .unwrap()
            .iter()
            .all(|message| !message
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("secret-token"))
    );
    service.shutdown().await;
}
