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

use crate::{Services, runtime::RuntimeState};

use super::{
    routes,
    service::{
        CommandFuture, CommandOutput, CommandRunner, PasswordEnvironment, PasswordError,
        PasswordService, ProcessCommandRunner, store_path,
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
        let path = self.0.join("fake-gpg");
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
    calls: Mutex<Vec<Vec<String>>>,
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
        _program: &'a str,
        args: &'a [String],
        _input: Option<&'a [u8]>,
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.to_vec());
            if self.pending {
                return std::future::pending().await;
            }
            if args.iter().any(|arg| arg == "--show-only-session-key") {
                return Ok(CommandOutput {
                    status: Some(0),
                    stdout: b"[GNUPG:] SESSION_KEY 9:1111\n".to_vec(),
                });
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

fn configured(fixture: &Fixture, runner: Arc<FakeRunner>) -> Arc<PasswordService> {
    Arc::new(PasswordService::with_fixture_runner(
        Some(PasswordEnvironment::new(
            "/bin/echo".into(),
            fixture.0.clone(),
        )),
        runner,
        Duration::from_secs(30),
    ))
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
    fixture.entry(".hidden/secret.gpg");
    let runner = Arc::new(FakeRunner::default());
    let service = configured(&fixture, Arc::clone(&runner));
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
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn fetches_raw_contents_and_passes_a_checked_absolute_path_to_gpg() {
    let fixture = Fixture::new();
    fixture.entry("-c.gpg");
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), CONTENTS);
    let service = configured(&fixture, Arc::clone(&runner));
    let (status, body) = request(routes::router(service), "GET", "/password/entries/-c.gpg").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "file":"-c.gpg", "contents":CONTENTS})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(
        &calls[0][3..8],
        [
            "--status-fd",
            "1",
            "--show-only-session-key",
            "--decrypt",
            "--"
        ]
    );
    assert_eq!(calls[0].last(), calls[1].last());
    let snapshot = Path::new(calls[1].last().unwrap());
    assert!(snapshot.is_absolute());
    assert_eq!(snapshot.file_name().unwrap(), "entry.gpg");
    assert!(calls[1].iter().any(|arg| arg == "--decrypt"));
    assert!(!snapshot.exists());
}

#[tokio::test]
async fn reads_nested_and_percent_encoded_resource_paths_without_a_request_body() {
    let fixture = Fixture::new();
    fixture.entry("example.com/alice.gpg");
    fixture.entry("中文 网站/a#?%.gpg");
    std::fs::write(fixture.0.join("中文 网站/a#?%.gpg"), "另一份密文").unwrap();
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), CONTENTS);
    runner.output(Some(0), CONTENTS);
    let service = configured(&fixture, Arc::clone(&runner));
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
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].last(), calls[1].last());
    assert_eq!(calls[2].last(), calls[3].last());
    assert_ne!(calls[0].last(), calls[2].last());
}

#[tokio::test]
async fn decrypts_openpgp_in_rust_and_reuses_the_cached_session_key() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct Agent(AtomicUsize);

    impl CommandRunner for Agent {
        fn run<'a>(
            &'a self,
            _: &'a str,
            args: &'a [String],
            _: Option<&'a [u8]>,
        ) -> CommandFuture<'a> {
            Box::pin(async move {
                assert!(args.iter().any(|arg| arg == "--show-only-session-key"));
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(CommandOutput {
                    status: Some(0),
                    stdout: b"[GNUPG:] SESSION_KEY 9.2:B9B7DE7BD4ADCA47DF9E7C7BF37829E910AFB6F14E799B67D0C268D1331C9464\n".to_vec(),
                })
            })
        }
    }

    // 用一次性密钥生成的压缩 OpenPGP 密文。
    let ciphertext = STANDARD.decode("hQEMA469UUVI/69EAQf/fu7nwhRSpK6ZyHn12o9cnAmgKQzC/9C1jtFU372yu6j1Tx2hafqRtMDMh0EVKc2C9L1lG7E18kTYoWIWjKHyek+dR/BF26bKPGS2G9VmmtyO8DPolFq12WS/erLMgJmjHczXhjeU5B5+33JLFm/4ZdXXZOivNHCgU/4Xo1MzxFTuIk/XhKw6B1e6pQcjnlxtbV0tdlz/+stJm5jiRdVhJ1vAFGl7r8P5jCAqiyW6XNWGtclAiMv7PXtFRIM0zs+lgdEjrpwhL7sUrWt+3wHyJehbWZ69X5jnx5KjdIPp1wkWVeMVYOzqURSXDraPko2AygfcjXsmrw/U0Q6wjxgm4NRNAQkCEGqpWLdOQ/n3UAFrjhxGMO3C+iaUAMo/h8wdSgX4ZozcS4zBMK2t59gidSMn4ctpNlT4wbIS+MNpEznFfnr9KsHuzjFYVsna5T8=").unwrap();
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("entry.gpg"), &ciphertext).unwrap();
    let agent = Arc::new(Agent::default());
    let service = PasswordService::with_runner(
        Some(PasswordEnvironment::new(
            "/bin/echo".into(),
            fixture.0.clone(),
        )),
        agent.clone(),
        Duration::from_secs(1),
    );
    let (left, right) = tokio::join!(service.fetch("entry.gpg"), service.fetch("entry.gpg"));
    assert_eq!(left.unwrap(), "fixture\n");
    assert_eq!(right.unwrap(), "fixture\n");
    assert_eq!(agent.0.load(Ordering::SeqCst), 1);

    let mut corrupt = ciphertext;
    *corrupt.last_mut().unwrap() ^= 1;
    std::fs::write(fixture.0.join("entry.gpg"), corrupt).unwrap();
    assert!(matches!(
        service.fetch("entry.gpg").await,
        Err(PasswordError::InvalidOutput)
    ));
    assert_eq!(agent.0.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn rejects_traversal_options_hidden_paths_and_missing_files_before_running_gpg() {
    let fixture = Fixture::new();
    let runner = Arc::new(FakeRunner::default());
    let service = configured(&fixture, Arc::clone(&runner));
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
    let service = configured(&fixture, Arc::clone(&runner));
    assert!(service.entries("").await.unwrap().is_empty());
    for file in ["linked.gpg", "external/secret.gpg"] {
        assert!(matches!(
            service.fetch(file).await,
            Err(PasswordError::InvalidPath)
        ));
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[test]
fn uses_the_password_store_environment() {
    let fixture = Fixture::new();
    let home = fixture.0.as_os_str().to_owned();
    let custom = fixture.0.join("custom-store");
    let store = store_path(Some(custom.clone().into_os_string()), Some(home.clone())).unwrap();
    assert_eq!(store, custom);
    for unset in [None, Some(std::ffi::OsString::new())] {
        assert_eq!(
            store_path(unset, Some(home.clone())).unwrap(),
            fixture.0.join(".password-store")
        );
    }
}

#[tokio::test]
async fn stores_gpg_output_using_the_nearest_recipients_and_keeps_old_ciphertext_on_failure() {
    let fixture = Fixture::new();
    fixture.entry("team/nested/entry.gpg");
    std::fs::write(fixture.0.join(".gpg-id"), "root@example.org\n").unwrap();
    std::fs::write(
        fixture.0.join("team/.gpg-id"),
        "# 团队收件人\nteam@example.org # 注释\n",
    )
    .unwrap();
    let runner = Arc::new(FakeRunner::default());
    runner.output(Some(0), b"test-ciphertext");
    runner.output(Some(1), b"partial-ciphertext");
    let service = configured(&fixture, Arc::clone(&runner));
    let file = "team/nested/entry.gpg";
    service.save(file, "测试密码").await.unwrap();
    assert!(matches!(
        service.save(file, "测试新密码").await,
        Err(PasswordError::CommandFailed { .. })
    ));
    assert_eq!(
        std::fs::read(fixture.0.join(file)).unwrap(),
        b"test-ciphertext"
    );
    assert_eq!(
        std::fs::metadata(fixture.0.join(file))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let calls = runner.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .all(|args| args.ends_with(&["--recipient".into(), "team@example.org".into()]))
    );
}

#[tokio::test]
async fn hides_internal_credentials_and_their_aliases_from_http() {
    let fixture = Fixture::new();
    let entry = "loki-assistant/network/identity.gpg";
    fixture.entry(entry);
    symlink(fixture.0.join(entry), fixture.0.join("alias.gpg")).unwrap();
    let runner = Arc::new(FakeRunner::default());
    let service = configured(&fixture, Arc::clone(&runner));
    let app = routes::router(service);
    assert_eq!(
        request(app.clone(), "GET", "/password/entries").await.1["files"],
        json!([])
    );
    for file in [entry, "alias.gpg"] {
        assert_eq!(
            request(app.clone(), "GET", &entry_uri(file)).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn reports_missing_configuration_and_unavailable_store_without_running_gpg() {
    let runner = Arc::new(FakeRunner::default());
    let service = Arc::new(PasswordService::with_fixture_runner(
        None,
        runner.clone(),
        Duration::from_secs(1),
    ));
    let (status, body) = request(routes::router(service), "GET", "/password/entries").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "password-not-configured");
    let fixture = Fixture::new();
    let service = configured(&fixture, Arc::clone(&runner));
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
    let service = configured(&fixture, runner);
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
}

#[tokio::test(start_paused = true)]
async fn times_out_gpg_decryption() {
    let fixture = Fixture::new();
    fixture.entry("entry.gpg");
    let runner = Arc::new(FakeRunner {
        pending: true,
        ..Default::default()
    });
    let service = configured(&fixture, runner);
    let (status, body) = request(
        routes::router(service),
        "GET",
        "/password/entries/entry.gpg",
    )
    .await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "pass-timeout");
}

#[tokio::test]
async fn rejects_invalid_requests_and_write_routes_without_invoking_gpg() {
    let fixture = Fixture::new();
    let runner = Arc::new(FakeRunner::default());
    let service = configured(&fixture, Arc::clone(&runner));
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
async fn process_runner_preserves_stdout_and_pipes_input() {
    let fixture = Fixture::new();
    let program = fixture
        .executable("#!/bin/sh\n[ \"$LC_ALL\" = C ] || exit 1\ncat\nprintf '敏感诊断内容' >&2\n");
    let output = ProcessCommandRunner
        .run(&program, &[], Some(b"secret\r\nlogin: user\n\n"))
        .await
        .unwrap();
    assert_eq!(output.status, Some(0));
    assert_eq!(output.stdout, b"secret\r\nlogin: user\n\n");
    let error = ProcessCommandRunner
        .run("/不存在的程序", &[], None)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, PasswordError::Process));
}

#[tokio::test]
async fn registers_discovery_and_routes_without_password_configuration() {
    let services = Arc::new(Services::build());
    let app = services.router(Arc::new(RuntimeState::new(Arc::clone(&services))));
    for path in ["/meta/password", "/discover/modules/password"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        if path.starts_with("/meta") {
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
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
    let (status, body) = request(app, "GET", "/password/entries").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "password-not-configured");
}
