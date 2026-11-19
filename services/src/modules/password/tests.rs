use std::{
    collections::VecDeque,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{Services, modules::config::service::ConfigService, runtime::RuntimeState};

use super::{
    config::PasswordConfig,
    routes,
    service::{
        CommandFuture, CommandOutput, CommandRunner, PasswordError, PasswordService,
        ProcessCommandRunner,
    },
};

const CONTENTS: &str = "测试密码\r\nlogin: alice\r\nurl: https://example.com\r\notp: TEST\r\n\r\n";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("loki-password-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }

    fn entry(&self, file: &str) {
        let path = self.0.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "测试替身，不是真实密码").unwrap();
    }

    fn executable(&self, script: &str) -> String {
        let path = self.0.join("fake-pass");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.to_str().unwrap().to_owned()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Default)]
struct FakeRunner {
    calls: Mutex<Vec<(String, PathBuf, Vec<String>)>>,
    outputs: Mutex<VecDeque<(Option<i32>, Vec<u8>)>>,
    pending: bool,
}

impl FakeRunner {
    fn output(&self, status: Option<i32>, stdout: impl AsRef<[u8]>) {
        self.outputs
            .lock()
            .unwrap()
            .push_back((status, stdout.as_ref().to_vec()));
    }
}

impl CommandRunner for FakeRunner {
    fn run<'a>(
        &'a self,
        program: &'a str,
        store: &'a Path,
        args: &'a [String],
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_owned(), store.to_owned(), args.to_vec()));
            if self.pending {
                return std::future::pending().await;
            }
            let (status, stdout) = self
                .outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("应预先提供进程输出");
            Ok(CommandOutput { status, stdout })
        })
    }
}

async fn configured(
    fixture: &Fixture,
    runner: Arc<FakeRunner>,
) -> (Arc<ConfigService>, Arc<PasswordService>) {
    let config = Arc::new(ConfigService::new(PasswordConfig::metadata()));
    config
        .replace("password", "store_dir", json!(fixture.0.to_str().unwrap()))
        .await
        .unwrap();
    // 替身仅记录参数，配置校验不会执行该程序。
    config
        .replace("password", "pass_path", json!("/bin/echo"))
        .await
        .unwrap();
    let service = Arc::new(PasswordService::with_runner(
        PasswordConfig::new(Arc::clone(&config)),
        runner,
        Duration::from_secs(30),
    ));
    (config, service)
}

async fn request(app: Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

fn entry_uri(file: &str) -> String {
    format!(
        "/password/entries/{}",
        percent_encoding::utf8_percent_encode(file, percent_encoding::NON_ALPHANUMERIC)
    )
}

#[tokio::test]
async fn lists_nested_entries_without_decrypting_and_filters_paths() {
    let fixture = Fixture::new();
    fixture.entry("Example.COM/alice.gpg");
    fixture.entry("中文 网站/账号.gpg");
    fixture.entry("double.gpg.gpg");
    fixture.entry("directory.gpg/nested.gpg");
    fixture.entry("directory/first.gpg");
    fixture.entry("name -> text.gpg");
    std::fs::create_dir(fixture.0.join("empty")).unwrap();
    std::fs::write(fixture.0.join("notes.txt"), "不是密码条目").unwrap();
    let listing = "Password Store\n|-- \u{1b}[01;34mExample.COM\u{1b}[0m\n|   `-- \u{1b}[00malice\u{1b}[0m\n|-- directory\n|   `-- first\n|-- directory\n|   `-- nested\n|-- double.gpg\n|-- empty\n|-- name -> text\n|-- notes.txt\n`-- 中文 网站\n    `-- 账号\n";
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), listing);
    runner.output(Some(0), listing);
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    assert_eq!(
        service.entries("").await.unwrap(),
        vec![
            "Example.COM/alice.gpg",
            "directory.gpg/nested.gpg",
            "directory/first.gpg",
            "double.gpg.gpg",
            "name -> text.gpg",
            "中文 网站/账号.gpg"
        ]
    );
    let (status, body) = request(
        routes::router(service),
        "GET",
        "/password/entries?query=example.com",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "files":["Example.COM/alice.gpg"]})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(_, store, args)| store == &fixture.0 && args == &["ls"])
    );
}

#[tokio::test]
async fn fetches_raw_contents_and_uses_fixed_show_with_option_separator() {
    let fixture = Fixture::new();
    fixture.entry("-c.gpg");
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), CONTENTS);
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    let (status, body) = request(routes::router(service), "GET", "/password/entries/-c.gpg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "file":"-c.gpg", "contents":CONTENTS})
    );
    assert_eq!(runner.calls.lock().unwrap()[0].2, vec!["show", "--", "-c"]);
}

#[tokio::test]
async fn reads_nested_and_percent_encoded_resource_paths_without_a_request_body() {
    let fixture = Fixture::new();
    fixture.entry("example.com/alice.gpg");
    fixture.entry("中文 网站/a#?%.gpg");
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), CONTENTS);
    runner.output(Some(0), CONTENTS);
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    for (file, uri) in [
        (
            "example.com/alice.gpg",
            "/password/entries/example.com/alice.gpg".to_owned(),
        ),
        ("中文 网站/a#?%.gpg", entry_uri("中文 网站/a#?%.gpg")),
    ] {
        let (status, body) = request(routes::router(Arc::clone(&service)), "GET", &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["file"], file);
        assert_eq!(body["contents"], CONTENTS);
    }
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].2, vec!["show", "--", "example.com/alice"]);
    assert_eq!(calls[1].2, vec!["show", "--", "中文 网站/a#?%"]);
}

#[tokio::test]
async fn rejects_traversal_options_hidden_paths_and_missing_files_before_running_pass() {
    let fixture = Fixture::new();
    let runner = Arc::new(FakeRunner::default());
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    assert!(matches!(
        service.fetch("").await,
        Err(PasswordError::InvalidPath)
    ));
    for file in [
        "/tmp/file.gpg",
        "../file.gpg",
        "a/../file.gpg",
        "a/./file.gpg",
        "a//file.gpg",
        "a\\file.gpg",
        "a\n.gpg",
        "a\0.gpg",
        ".git/config.gpg",
        "plain.txt",
        "directory/",
    ] {
        let (status, body) = request(
            routes::router(Arc::clone(&service)),
            "GET",
            &entry_uri(file),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{file:?}");
        assert_eq!(body["error"]["code"], "invalid-password-path");
    }
    let (status, body) = request(
        routes::router(service),
        "GET",
        "/password/entries/missing.gpg",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "password-entry-not-found");
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn excludes_and_rejects_symlinks_outside_the_configured_store() {
    let fixture = Fixture::new();
    let outside = Fixture::new();
    outside.entry("secret.gpg");
    symlink(outside.0.join("secret.gpg"), fixture.0.join("linked.gpg")).unwrap();
    symlink(&outside.0, fixture.0.join("external")).unwrap();
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), "Password Store\n|-- external -> /outside\n|   `-- secret\n`-- linked -> /outside/secret.gpg\n");
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    assert!(service.entries("").await.unwrap().is_empty());
    for file in ["linked.gpg", "external/secret.gpg"] {
        assert!(matches!(
            service.fetch(file).await,
            Err(PasswordError::InvalidPath)
        ));
    }
    assert_eq!(runner.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn configuration_changes_take_effect_on_the_next_query() {
    let fixture = Fixture::new();
    fixture.entry("old.gpg");
    let next = Fixture::new();
    next.entry("new.gpg");
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), "old\n");
    runner.output(Some(0), "new\n");
    let (config, service) = configured(&fixture, Arc::clone(&runner)).await;
    assert_eq!(service.fetch("old.gpg").await.unwrap(), "old\n");
    config
        .replace("password", "store_dir", json!(next.0.to_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(service.fetch("new.gpg").await.unwrap(), "new\n");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].1, fixture.0);
    assert_eq!(calls[1].1, next.0);
}

#[tokio::test]
async fn reports_missing_configuration_and_unavailable_store_without_running_pass() {
    let config = Arc::new(ConfigService::new(PasswordConfig::metadata()));
    let runner = Arc::new(FakeRunner::default());
    let service = Arc::new(PasswordService::with_runner(
        PasswordConfig::new(Arc::clone(&config)),
        runner.clone(),
        Duration::from_secs(1),
    ));
    let (status, body) = request(routes::router(service), "GET", "/password/entries").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "password-not-configured");
    let fixture = Fixture::new();
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    std::fs::remove_dir_all(&fixture.0).unwrap();
    assert!(matches!(
        service.entries("").await,
        Err(PasswordError::StoreUnavailable)
    ));
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn converts_exit_status_and_invalid_outputs_without_echoing_secrets() {
    let fixture = Fixture::new();
    fixture.entry("entry.gpg");
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(2), CONTENTS);
    runner.output(Some(0), [0xff]);
    runner.output(Some(0), "Password Store\nnot-a-tree\n");
    runner.output(Some(0), "Password Store\n");
    let (_, service) = configured(&fixture, runner).await;
    let (status, body) = request(
        routes::router(Arc::clone(&service)),
        "GET",
        "/password/entries/entry.gpg",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["api_version"], "1");
    assert_eq!(body["status"], 502);
    assert_eq!(body["error"]["source"], "services");
    assert_eq!(body["error"]["code"], "pass-command-failed");
    assert!(!body.to_string().contains("测试密码"));
    assert!(matches!(
        service.fetch("entry.gpg").await,
        Err(PasswordError::InvalidOutput)
    ));
    assert!(matches!(
        service.entries("").await,
        Err(PasswordError::InvalidOutput)
    ));
    assert!(service.entries("").await.unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn times_out_pass_queries() {
    let fixture = Fixture::new();
    let runner = Arc::new(FakeRunner {
        pending: true,
        ..Default::default()
    });
    let (_, service) = configured(&fixture, runner).await;
    let (status, body) = request(routes::router(service), "GET", "/password/entries").await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "pass-timeout");
}

#[tokio::test]
async fn rejects_invalid_requests_and_write_routes_without_invoking_pass() {
    let fixture = Fixture::new();
    let runner = Arc::new(FakeRunner::default());
    let (_, service) = configured(&fixture, Arc::clone(&runner)).await;
    for uri in [
        "/password/entries?query=%00",
        "/password/entries?store_dir=/tmp",
        "/password/entries/%FF.gpg",
    ] {
        let (status, response) = request(routes::router(Arc::clone(&service)), "GET", uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]["message"].is_string());
    }
    for uri in ["/password/entries", "/password/entries/entry.gpg"] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let (status, response) =
                request(routes::router(Arc::clone(&service)), method, uri).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response["error"]["code"], "method-not-allowed");
        }
    }
    let response = routes::router(service)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/password/fetch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn process_runner_passes_store_and_keeps_stdout_exact_and_stderr_private() {
    let fixture = Fixture::new();
    fixture.entry("entry.gpg");
    let program = fixture.executable("#!/bin/sh\n[ \"$PASSWORD_STORE_ENABLE_EXTENSIONS\" = false ] || exit 10\n[ \"$LC_ALL\" = C ] || exit 11\n[ -f \"$PASSWORD_STORE_DIR/entry.gpg\" ] || exit 12\n[ \"$1\" = show ] && [ \"$2\" = -- ] && [ \"$3\" = entry ] || exit 13\nprintf 'secret\\r\\nlogin: user\\n\\n'\nprintf '敏感诊断内容' >&2\n");
    let output = ProcessCommandRunner
        .run(
            &program,
            &fixture.0,
            &["show".to_owned(), "--".to_owned(), "entry".to_owned()],
        )
        .await
        .unwrap();
    assert_eq!(output.status, Some(0));
    assert_eq!(output.stdout, b"secret\r\nlogin: user\n\n");
    let error = ProcessCommandRunner
        .run("/不存在的程序", &fixture.0, &[])
        .await
        .err()
        .unwrap();
    assert!(matches!(error, PasswordError::Process));
}

#[tokio::test]
async fn registers_config_discovery_and_routes_in_the_composition_root() {
    let services = Arc::new(Services::build());
    let app = services.router(Arc::new(RuntimeState::new(Arc::clone(&services))));
    for path in ["/meta/password", "/discover/modules/password"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        if path.starts_with("/meta") {
            let metadata = body.to_string();
            assert!(metadata.contains("store_dir"));
            assert!(metadata.contains("pass_path"));
        } else {
            assert_eq!(body["apis"].as_array().unwrap().len(), 2);
            assert!(
                body["apis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|api| api["method"] == "GET")
            );
            let entry = body["apis"]
                .as_array()
                .unwrap()
                .iter()
                .find(|api| api["path"] == "/password/entries/{*file}")
                .unwrap();
            assert_eq!(entry["parameters"][0]["name"], "file");
            assert_eq!(entry["parameters"][0]["location"], "path");
            assert_eq!(entry["parameters"][0]["required"], true);
        }
    }
    let (status, body) = request(app, "GET", "/password/entries").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "password-not-configured");
}
