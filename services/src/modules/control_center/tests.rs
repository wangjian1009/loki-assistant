use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::{
    modules::config::{self, service::ConfigService},
    platform::macos::{CommandOutput, CommandRunner, LaunchdError, LaunchdManager},
};

#[derive(Debug, Default)]
struct State {
    loaded: bool,
    fail: Option<String>,
    calls: Vec<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
struct FakeRunner(Arc<Mutex<State>>);

impl CommandRunner for FakeRunner {
    fn run(&self, program: &Path, args: &[String]) -> Result<CommandOutput, LaunchdError> {
        assert_eq!(program, Path::new("/bin/launchctl"));
        let mut state = self.0.lock().unwrap();
        state.calls.push(args.to_vec());
        let success = if state.fail.as_deref() == Some(&args[0]) {
            false
        } else {
            match args[0].as_str() {
                "print" => state.loaded,
                "bootstrap" => {
                    state.loaded = true;
                    true
                }
                "bootout" => {
                    state.loaded = false;
                    true
                }
                _ => true,
            }
        };
        Ok(CommandOutput {
            success,
            status: Some(if success { 0 } else { 1 }),
            stderr: if success {
                String::new()
            } else {
                "系统拒绝操作".to_owned()
            },
            stdout: String::new(),
        })
    }
}

struct Fixture {
    root: PathBuf,
    executable: PathBuf,
    runner: FakeRunner,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("loki-center-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let executable = root.join("含空格 & 托盘");
        fs::write(&executable, "测试文件，不执行").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            executable,
            runner: FakeRunner::default(),
        }
    }

    fn service(&self, path: &Path) -> ConfigService {
        ConfigService::load(path).unwrap().with_change_handler(
            "control-center",
            Arc::new(ControlCenterService::with_manager(
                LaunchdManager::for_user(&self.root, 123, self.runner.clone()),
                self.executable.clone(),
                self.root.clone(),
            )),
        )
    }

    fn plist(&self) -> PathBuf {
        self.root
            .join("Library/LaunchAgents/cc.loki-assistant.center.plist")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[tokio::test]
async fn config_commits_apply_login_startup_and_repeat_safely() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    let service = fixture.service(&path);
    assert!(!fixture.plist().exists());
    assert!(fixture.runner.0.lock().unwrap().calls.is_empty());
    let mut changes = service.subscribe();
    for enabled in [true, true, false, false] {
        service
            .replace("control-center", "center_auto_start", json!(enabled))
            .await
            .unwrap();
        assert_eq!(changes.recv().await.unwrap().value, json!(enabled));
        assert_eq!(fixture.plist().exists(), enabled);
        assert_eq!(fixture.runner.0.lock().unwrap().loaded, enabled);
        assert_eq!(
            ConfigService::load(&path)
                .unwrap()
                .value("control-center", "center_auto_start")
                .await
                .unwrap()
                .value,
            json!(enabled)
        );
        if enabled {
            let plist = fs::read_to_string(fixture.plist()).unwrap();
            assert!(plist.contains("含空格 &amp; 托盘"));
            assert!(plist.contains("<key>LimitLoadToSessionType</key><string>Aqua</string>"));
            assert!(plist.contains("<key>KeepAlive</key><false/>"));
        }
    }
    let state = fixture.runner.0.lock().unwrap();
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|args| args[0] == "bootstrap")
            .count(),
        1
    );
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|args| args[0] == "kickstart")
            .count(),
        1
    );
    assert_eq!(
        state
            .calls
            .iter()
            .filter(|args| args[0] == "bootout")
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_handler_returns_http_error_without_modifying_config_or_notifying() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    let service = fixture.service(&path);
    service
        .replace("control-center", "center_auto_start", json!(false))
        .await
        .unwrap();
    let original = fs::read(&path).unwrap();
    fixture.runner.0.lock().unwrap().fail = Some("bootstrap".to_owned());
    let mut changes = service.subscribe();
    let module = config::Module::from_service(service);
    let response = module
        .router()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/data/control-center/center_auto_start")
                .header("content-type", "application/json")
                .body(Body::from("true"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    let error: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(error["code"], "config-change-rejected");
    assert!(error["message"].as_str().unwrap().contains("系统拒绝操作"));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        module
            .service()
            .value("control-center", "center_auto_start")
            .await
            .unwrap()
            .value,
        json!(false)
    );
    assert!(!fixture.plist().exists());
    assert!(!fixture.runner.0.lock().unwrap().loaded);
    assert!(changes.try_recv().is_err());
}

#[tokio::test]
async fn failed_disable_preserves_existing_plist_and_value() {
    let fixture = Fixture::new();
    let path = fixture.root.join("config.yml");
    let service = fixture.service(&path);
    service
        .replace("control-center", "center_auto_start", json!(true))
        .await
        .unwrap();
    let original = fs::read(fixture.plist()).unwrap();
    fixture.runner.0.lock().unwrap().fail = Some("bootout".to_owned());
    assert!(
        service
            .replace("control-center", "center_auto_start", json!(false))
            .await
            .is_err()
    );
    assert_eq!(fs::read(fixture.plist()).unwrap(), original);
    assert!(fixture.runner.0.lock().unwrap().loaded);
    assert_eq!(
        service
            .value("control-center", "center_auto_start")
            .await
            .unwrap()
            .value,
        json!(true)
    );
}

#[tokio::test]
async fn missing_executable_rejects_enable_but_allows_disable() {
    let fixture = Fixture::new();
    let service = fixture.service(&fixture.root.join("config.yml"));
    fs::remove_file(&fixture.executable).unwrap();
    assert!(
        service
            .replace("control-center", "center_auto_start", json!(true))
            .await
            .is_err()
    );
    service
        .replace("control-center", "center_auto_start", json!(false))
        .await
        .unwrap();
    assert!(!fixture.plist().exists());
}

#[tokio::test]
async fn storage_failure_reverts_applied_login_startup() {
    let fixture = Fixture::new();
    let parent = fixture.root.join("blocked");
    let service = fixture.service(&parent.join("config.yml"));
    fs::write(&parent, "阻止配置落盘").unwrap();
    let mut changes = service.subscribe();
    assert!(matches!(
        service
            .replace("control-center", "center_auto_start", json!(true))
            .await,
        Err(config::service::ConfigServiceError::Storage(_))
    ));
    assert!(!fixture.plist().exists());
    assert!(!fixture.runner.0.lock().unwrap().loaded);
    assert_eq!(
        service
            .value("control-center", "center_auto_start")
            .await
            .unwrap()
            .value,
        json!(false)
    );
    assert!(changes.try_recv().is_err());
}
