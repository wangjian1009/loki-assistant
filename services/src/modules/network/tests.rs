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
use serde_json::json;

use super::{
    IdentityStore, IrohRelayRegistrar, NetworkService, RelayRegistrar, RelaySession,
    relay::RelayFuture,
};
use crate::{
    modules::{
        config::{
            self,
            service::{ConfigService, ConfigServiceError},
        },
        notification::NotificationService,
    },
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

#[derive(Debug, Default)]
struct Launcher {
    messages: Mutex<Vec<NotificationContent>>,
    fail: AtomicBool,
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
    fn close(&self) -> RelayFuture<'_, ()> {
        Box::pin(async {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        })
    }
}

#[derive(Debug, Default)]
struct Registrar {
    calls: Mutex<Vec<(String, String)>>,
    sessions: Mutex<Vec<Arc<Session>>>,
    fail: AtomicBool,
    hang: AtomicBool,
}
impl RelayRegistrar for Registrar {
    fn register(&self, identity: SecretKey, url: String) -> RelayFuture<'_, Arc<dyn RelaySession>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((url, identity.public().to_string()));
            if self.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err("中转拒绝注册".to_owned());
            }
            let session = Arc::new(Session::default());
            self.sessions.lock().unwrap().push(session.clone());
            Ok(session as Arc<dyn RelaySession>)
        })
    }
}

struct Fixture {
    root: PathBuf,
    registrar: Arc<Registrar>,
    launcher: Arc<Launcher>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("loki-network-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            registrar: Arc::default(),
            launcher: Arc::default(),
        }
    }
    fn store(&self) -> IdentityStore {
        IdentityStore::new(self.root.join("identity.json"))
    }
    fn service(&self) -> Arc<NetworkService> {
        Arc::new(NetworkService::new(
            Ok(self.store()),
            self.registrar.clone(),
            Arc::new(NotificationService::with_launcher(
                self.launcher.clone(),
                Duration::from_secs(1),
            )),
            Duration::from_millis(30),
        ))
    }
    fn config(&self, service: Arc<NetworkService>) -> Arc<ConfigService> {
        Arc::new(
            ConfigService::load(self.root.join("config.yml"))
                .unwrap()
                .with_change_handler("network", service),
        )
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
    let first_store = fixture.store();
    let second_store = fixture.store();
    let (first, second) = tokio::join!(first_store.ensure(), second_store.ensure());
    let (first, created_first) = first.unwrap();
    let (second, created_second) = second.unwrap();
    assert_ne!(created_first, created_second);
    assert_eq!(first.public(), second.public());
    let (loaded, created) = fixture.store().ensure().await.unwrap();
    assert!(!created);
    assert_eq!(loaded.public(), first.public());
    assert_eq!(
        fs::metadata(fixture.root.join("identity.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let other = Fixture::new();
    assert_ne!(
        other.store().ensure().await.unwrap().0.public(),
        first.public()
    );
}

#[tokio::test]
async fn corrupt_or_symlinked_identity_is_not_overwritten() {
    let fixture = Fixture::new();
    let path = fixture.root.join("identity.json");
    fs::write(&path, "损坏的身份").unwrap();
    assert!(fixture.store().ensure().await.is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "损坏的身份");
    fs::remove_file(&path).unwrap();
    let target = fixture.root.join("target");
    fs::write(&target, "保留原文件").unwrap();
    symlink(&target, &path).unwrap();
    assert!(fixture.store().ensure().await.is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "保留原文件");
}

#[tokio::test]
async fn startup_without_relay_generates_identity_once_and_never_connects() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    service.start(config.clone()).await.unwrap();
    service.start(config).await.unwrap();
    assert_eq!(fixture.titles(), ["已生成本地身份"]);
    assert!(fixture.registrar.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn saving_relay_creates_identity_notifies_and_keeps_connection_until_replaced() {
    let fixture = Fixture::new();
    let service = fixture.service();
    let config = fixture.config(service.clone());
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
        "network:\n  relay_url: https://saved.example\n",
    )
    .unwrap();
    let service = fixture.service();
    let config = fixture.config(service.clone());
    fixture.registrar.fail.store(true, Ordering::SeqCst);
    assert!(service.start(config.clone()).await.is_err());
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
    restarted.start(config).await.unwrap();
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
    assert_eq!(body["code"], "config-change-rejected");
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
    let service = fixture.service();
    let path = fixture.root.join("blocked/config.yml");
    let config = ConfigService::load(&path)
        .unwrap()
        .with_change_handler("network", service.clone());
    fs::write(path.parent().unwrap(), "阻止落盘").unwrap();
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
    fs::write(fixture.root.join("identity.json"), "损坏").unwrap();
    assert!(service.start(config).await.is_err());
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
        Ok(fixture.store()),
        Arc::new(IrohRelayRegistrar::new(Some(token))),
        Arc::new(NotificationService::with_launcher(
            fixture.launcher.clone(),
            Duration::from_secs(1),
        )),
        Duration::from_secs(10),
    ));
    let config = fixture.config(service.clone());
    config
        .replace("network", "relay_url", json!(address))
        .await
        .unwrap();
    assert_eq!(fixture.titles(), ["已生成本地身份", "中转注册成功"]);
    service.shutdown().await;
    let denied = IrohRelayRegistrar::new(Some(uuid::Uuid::new_v4().to_string()));
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        denied.register(SecretKey::generate(), address),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.contains("认证"));
    assert!(error.contains("令牌不匹配"));
    relay.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_relay_urls_are_rejected_without_fallback_connections() {
    let registrar = IrohRelayRegistrar::new(None);
    for url in [
        "not-a-url",
        "file:///tmp/relay",
        "ftp://relay.example",
        "http://user:password@127.0.0.1",
        "https://relay.example/?token=private",
    ] {
        assert!(
            registrar
                .register(SecretKey::generate(), url.to_owned())
                .await
                .is_err()
        );
    }
}
