use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::platform::macos::notification::{
    NotificationContent, NotificationFuture, NotificationLauncher,
};
use crate::{modules::config::service::ConfigService, runtime::RuntimeState};

use crate::modules::notification::{self, NotificationError, NotificationService};
use crate::modules::status::StatusService;

use super::{
    config::EmacsConfig,
    protocol::Notification,
    routes,
    service::{CommandOutput, CommandRunner, EmacsError, EmacsService, ServiceFuture},
};

#[derive(Debug, Default)]
struct FakeRunner {
    calls: Mutex<Vec<(String, Vec<String>)>>,
    outputs: Mutex<VecDeque<Result<CommandOutput, EmacsError>>>,
}

impl CommandRunner for FakeRunner {
    fn run<'a>(&'a self, program: &'a str, args: &'a [String]) -> ServiceFuture<'a, CommandOutput> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push((program.to_owned(), args.to_vec()));
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .expect("必须提供模拟输出")
        })
    }
}

#[derive(Debug, Default)]
struct FakeNotificationLauncher {
    calls: Mutex<Vec<NotificationContent>>,
    error: Option<String>,
}

impl NotificationLauncher for FakeNotificationLauncher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        self.calls.lock().unwrap().push(content);
        Box::pin(async { self.error.clone().map_or(Ok(()), Err) })
    }
}

fn notifications(launcher: Arc<dyn NotificationLauncher>) -> Arc<NotificationService> {
    Arc::new(NotificationService::with_launcher(
        launcher,
        Duration::from_millis(100),
    ))
}

fn default_status() -> Arc<StatusService> {
    Arc::new(StatusService::new())
}

fn default_notifications() -> Arc<NotificationService> {
    notifications(Arc::new(FakeNotificationLauncher::default()))
}

fn wire(envelope: Value) -> CommandOutput {
    CommandOutput {
        success: true,
        status: Some(0),
        stdout: serde_json::to_string(&envelope.to_string()).unwrap(),
        stderr: String::new(),
    }
}

fn conversation_reply(value: Value) -> Result<CommandOutput, EmacsError> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    Ok(wire(json!({
        "result": serde_json::to_string(&STANDARD.encode(value.to_string())).unwrap()
    })))
}

fn assert_conversation_lisp(args: &[String]) {
    let (_, wrapped_source) = args[3].split_once("(let* ((source ").unwrap();
    let source = serde_json::Deserializer::from_str(wrapped_source)
        .into_iter::<String>()
        .next()
        .unwrap()
        .unwrap();
    // 两个参数均为 Base64，Lisp 源码不需要反斜线转义。
    assert!(!source.contains('\\'));
}

#[tokio::test]
async fn agent_shell_stream_sends_snapshot_increment_and_closure() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let session_id = "*中文 会话/\"quoted\"..*";
    let encoded = "%2A%E4%B8%AD%E6%96%87%20%E4%BC%9A%E8%AF%9D%2F%22quoted%22%2E%2E%2A";
    let group = json!({
        "id": "activity-1", "kind": "group", "title": "运行命令", "text": "",
        "collapsed": true
    });
    let update = json!([
        {"id": "activity-1", "kind": "group", "title": "运行命令", "text": "", "collapsed": false},
        {"id": "tool-1", "kind": "tool", "text": "第一行", "parent_id": "activity-1", "collapsed": true},
        {"id": "tool-1:1", "kind": "continuation", "text": "第二行", "parent_id": "tool-1"},
        {"id": "message-1", "kind": "assistant", "text": "执行完成"}
    ]);
    let (emacs, runner, _) = service(vec![
        conversation_reply(json!({"kind": "snapshot", "revision": 0, "blocks": [group]})),
        conversation_reply(json!({"kind": "update", "revision": 1, "blocks": update})),
        conversation_reply(json!({"kind": "missing"})),
    ]);
    let response = routes::router(emacs)
        .oneshot(
            Request::builder()
                .uri(format!("/agent-shell/sessions/{encoded}/events"))
                .extension(crate::http::sse::RemoteSse)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let frames = to_bytes(response.into_body(), 4096).await.unwrap();
    let mut remaining = &frames[..];
    let mut messages = Vec::new();
    while !remaining.is_empty() {
        let length = u32::from_be_bytes(remaining[..4].try_into().unwrap()) as usize;
        let (json, tail) = remaining[4..].split_at(length);
        messages.push(serde_json::from_slice::<Value>(json).unwrap());
        remaining = tail;
    }
    assert_eq!(messages.len(), 3);
    assert!(messages.iter().all(|event| event["event"] == "agent-shell"));
    assert_eq!(
        messages[0]["data"],
        json!({
            "api_version": "1", "session_id": session_id,
            "kind": "snapshot", "revision": 0, "blocks": [group]
        })
    );
    assert_eq!(messages[1]["data"]["kind"], "update");
    assert_eq!(messages[1]["data"]["blocks"], update);
    assert_eq!(messages[2]["data"]["kind"], "closed");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    let lisp_argument = STANDARD.encode(session_id);
    for ((_, args), cursor) in calls.iter().zip(["nil", "0", "1"]) {
        assert!(args[3].contains(&format!(r#"base64-decode-string \"{lisp_argument}\""#)));
        assert!(args[3].contains(&format!(
            "loki-assistant-agent-shell-stream-read name {cursor})"
        )));
        assert!(!args[3].contains(session_id));
    }
}

#[tokio::test]
async fn agent_shell_stream_forwards_pushed_history_boundary_updates_and_closure() {
    use futures_util::StreamExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    #[derive(Debug)]
    struct PushRunner(Mutex<Option<tokio::sync::oneshot::Receiver<()>>>);

    impl CommandRunner for PushRunner {
        fn run<'a>(
            &'a self,
            _program: &'a str,
            args: &'a [String],
        ) -> ServiceFuture<'a, CommandOutput> {
            Box::pin(async move {
                // 通过受控 gateway 核对真实回环连接，第二次求值会使测试失败。
                let live = self.0.lock().unwrap().take().expect("只应调用一次 gateway");
                let pattern = regex::Regex::new(
                    r#"loki-assistant-agent-shell-stream-subscribe name (\d+) \\"([0-9a-f]+)\\""#,
                )
                .unwrap();
                let args = pattern.captures(&args[3]).unwrap();
                let port: u16 = args[1].parse().unwrap();
                let token = args[2].to_owned();
                tokio::spawn(async move {
                    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                    socket
                        .write_all(format!("{token}\n").as_bytes())
                        .await
                        .unwrap();
                    socket.write_all(b"{\"kind\":\"snapshot\",\"revision\":10,\"snapshot_end_revision\":11,\"blocks\":[]}\n").await.unwrap();
                    let mut ack = [0];
                    socket.read_exact(&mut ack).await.unwrap();
                    assert_eq!(ack, *b"\n");
                    socket
                        .write_all(b"{\"kind\":\"update\",\"revision\":11,\"blocks\":[]}\n")
                        .await
                        .unwrap();
                    socket.read_exact(&mut ack).await.unwrap();
                    live.await.unwrap();
                    socket
                        .write_all(b"{\"kind\":\"update\",\"revision\":12,\"blocks\":[]}\n")
                        .await
                        .unwrap();
                    socket.read_exact(&mut ack).await.unwrap();
                    socket
                        .write_all(b"{\"kind\":\"closed\",\"revision\":12}\n")
                        .await
                        .unwrap();
                });
                Ok(wire(json!({"result":"t"})))
            })
        }
    }

    let (publish, live) = tokio::sync::oneshot::channel();
    let emacs = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
        Arc::new(PushRunner(Mutex::new(Some(live)))),
        Duration::from_secs(1),
    ));
    let response = routes::router(emacs)
        .oneshot(
            Request::builder()
                .uri("/agent-shell/sessions/session/events")
                .extension(crate::http::sse::RemoteSse)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut frames = response.into_body().into_data_stream();
    let snapshot = frames.next().await.unwrap().unwrap();
    let snapshot: Value = serde_json::from_slice(&snapshot[4..]).unwrap();
    assert_eq!(snapshot["data"]["snapshot_end_revision"], 11);
    let replay = frames.next().await.unwrap().unwrap();
    let replay: Value = serde_json::from_slice(&replay[4..]).unwrap();
    assert_eq!(replay["data"]["revision"], 11);
    publish.send(()).unwrap();
    for (kind, revision) in [("update", 12), ("closed", 12)] {
        let update = frames.next().await.unwrap().unwrap();
        let update: Value = serde_json::from_slice(&update[4..]).unwrap();
        assert_eq!(update["data"]["kind"], kind);
        assert_eq!(update["data"]["revision"], revision);
    }
    assert!(frames.next().await.is_none());
}

#[tokio::test]
async fn agent_shell_stream_resume_opens_before_next_update() {
    use futures_util::StreamExt;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };

    #[derive(Debug)]
    struct ResumeRunner(Mutex<Option<tokio::sync::oneshot::Receiver<()>>>);

    impl CommandRunner for ResumeRunner {
        fn run<'a>(
            &'a self,
            _program: &'a str,
            args: &'a [String],
        ) -> ServiceFuture<'a, CommandOutput> {
            Box::pin(async move {
                let live = self.0.lock().unwrap().take().expect("只应调用一次 gateway");
                let pattern = regex::Regex::new(
                    r#"loki-assistant-agent-shell-stream-resume name (\d+) \\"([0-9a-f]+)\\" nil 12\)"#,
                )
                .unwrap();
                let args = pattern.captures(&args[3]).unwrap();
                let port: u16 = args[1].parse().unwrap();
                let token = args[2].to_owned();
                tokio::spawn(async move {
                    let mut socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                    socket
                        .write_all(format!("{token}\n").as_bytes())
                        .await
                        .unwrap();
                    live.await.unwrap();
                    socket
                        .write_all(b"{\"kind\":\"update\",\"revision\":13,\"blocks\":[]}\n")
                        .await
                        .unwrap();
                    let mut ack = [0];
                    socket.read_exact(&mut ack).await.unwrap();
                    assert_eq!(ack, *b"\n");
                    socket
                        .write_all(b"{\"kind\":\"closed\",\"revision\":13}\n")
                        .await
                        .unwrap();
                });
                Ok(wire(json!({"result":"resuming"})))
            })
        }
    }

    let (publish, live) = tokio::sync::oneshot::channel();
    let emacs = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
        Arc::new(ResumeRunner(Mutex::new(Some(live)))),
        Duration::from_secs(1),
    ));
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        routes::router(emacs).oneshot(
            Request::builder()
                .uri("/agent-shell/sessions/session/events?after=12")
                .extension(crate::http::sse::RemoteSse)
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("等待新内容时也应发送响应头")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    publish.send(()).unwrap();
    let mut frames = response.into_body().into_data_stream();
    for kind in ["update", "closed"] {
        let frame = frames.next().await.unwrap().unwrap();
        let data: Value = serde_json::from_slice(&frame[4..]).unwrap();
        assert_eq!(data["data"]["kind"], kind);
        assert_eq!(data["data"]["revision"], 13);
    }
    assert!(frames.next().await.is_none());
}

#[tokio::test]
async fn agent_shell_stream_keeps_recent_scope_across_revisions() {
    let (emacs, runner, _) = service(vec![
        conversation_reply(json!({"kind": "snapshot", "revision": 12, "blocks": []})),
        conversation_reply(json!({"kind": "update", "revision": 13, "blocks": []})),
        conversation_reply(json!({"kind": "missing"})),
    ]);
    let response = routes::router(emacs)
        .oneshot(
            Request::builder()
                .uri("/agent-shell/sessions/session/events?recent=10")
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stream = to_bytes(response.into_body(), 4096).await.unwrap();
    let events = std::str::from_utf8(&stream).unwrap();
    for kind in ["snapshot", "update", "closed"] {
        assert!(events.contains(&format!(r#""kind":"{kind}""#)));
    }
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    for ((_, args), cursor) in calls.iter().zip(["nil", "12", "13"]) {
        assert!(args[3].contains(&format!(
            "loki-assistant-agent-shell-stream-read name {cursor} 10)"
        )));
    }
}

#[tokio::test]
async fn agent_shell_messages_forward_exact_text_and_distinguish_emacs_outcomes() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let outcomes = [
        ("accepted", StatusCode::ACCEPTED, None),
        (
            "missing",
            StatusCode::NOT_FOUND,
            Some("agent-shell-session-not-found"),
        ),
        (
            "unavailable",
            StatusCode::SERVICE_UNAVAILABLE,
            Some("emacs-unavailable"),
        ),
    ];
    let (emacs, runner, _) = service(
        outcomes
            .iter()
            .map(|(kind, _, _)| conversation_reply(json!({"kind": kind})))
            .collect(),
    );
    let text = "  你好 \"手机\"\n  第二行\n";
    let route = "/agent-shell/sessions/%2A%E4%B8%AD%E6%96%87%2F%22quoted%22%2A/messages";
    for body in [
        json!({"text": " "}),
        json!({"text": "你好\0"}),
        json!({"text": "中".repeat(5500)}),
        json!({"text": "你好", "extra": true}),
    ] {
        let (status, reply) = request(emacs.clone(), "POST", route, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reply["error"]["code"], "invalid-request");
    }
    assert!(runner.calls.lock().unwrap().is_empty());

    for (_, expected, code) in outcomes {
        let (status, reply) = request(emacs.clone(), "POST", route, json!({"text": text})).await;
        assert_eq!(status, expected);
        if let Some(code) = code {
            assert_eq!(reply["error"]["code"], code);
            assert!(!reply.to_string().contains("第二行"));
        } else {
            assert_eq!(reply, json!({"api_version": "1", "accepted": true}));
        }
    }
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), outcomes.len());
    for (_, args) in calls.iter() {
        assert_conversation_lisp(args);
        assert!(args[3].contains("loki-assistant-agent-shell-send-message name argument"));
        assert!(args[3].contains(&STANDARD.encode("*中文/\"quoted\"*")));
        assert!(args[3].contains(&STANDARD.encode(text)));
        assert!(!args[3].contains("第二行"));
    }
}

#[tokio::test]
async fn agent_shell_restart_forwards_only_the_session_and_preserves_business_errors() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let cases = [
        ("accepted", StatusCode::ACCEPTED, None),
        (
            "busy",
            StatusCode::CONFLICT,
            Some("agent-shell-session-busy"),
        ),
        (
            "not_restartable",
            StatusCode::CONFLICT,
            Some("agent-shell-session-not-restartable"),
        ),
        (
            "missing",
            StatusCode::NOT_FOUND,
            Some("agent-shell-session-not-found"),
        ),
    ];
    let (emacs, runner, _) = service(
        cases
            .iter()
            .map(|(kind, _, _)| conversation_reply(json!({"kind": kind})))
            .collect(),
    );
    let route = "/agent-shell/sessions/%2A%E4%B8%AD%E6%96%87%2F%22quoted%22%2A/restart";
    for (_, expected, code) in cases {
        let response = routes::router(emacs.clone())
            .oneshot(Request::post(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let reply: Value = serde_json::from_slice(&body).unwrap();
        if let Some(code) = code {
            assert_eq!(reply["error"]["code"], code);
        } else {
            assert_eq!(reply, json!({"api_version": "1", "accepted": true}));
        }
    }
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), cases.len());
    for (_, args) in calls.iter() {
        assert_conversation_lisp(args);
        assert!(args[3].contains("loki-assistant-agent-shell-restart-session name)"));
        assert!(args[3].contains(&STANDARD.encode("*中文/\"quoted\"*")));
    }
}

#[tokio::test]
async fn agent_shell_clear_buffer_calls_the_fixed_emacs_entry() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let (emacs, runner, _) = service(vec![
        conversation_reply(json!({"kind": "accepted"})),
        conversation_reply(json!({"kind": "missing"})),
        conversation_reply(json!({"kind": "unavailable"})),
    ]);
    let route = "/agent-shell/sessions/%2A%E4%B8%AD%E6%96%87%2F%22quoted%22%2A/clear-buffer";
    for (status, code) in [
        (StatusCode::ACCEPTED, None),
        (StatusCode::NOT_FOUND, Some("agent-shell-session-not-found")),
        (StatusCode::SERVICE_UNAVAILABLE, Some("emacs-unavailable")),
    ] {
        let response = routes::router(emacs.clone())
            .oneshot(Request::post(route).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let reply: Value = serde_json::from_slice(&body).unwrap();
        if let Some(code) = code {
            assert_eq!(reply["error"]["code"], code);
        } else {
            assert_eq!(reply, json!({"api_version": "1", "accepted": true}));
        }
    }
    for (_, args) in runner.calls.lock().unwrap().iter() {
        assert_conversation_lisp(args);
        assert!(args[3].contains("loki-assistant-agent-shell-clear-buffer name)"));
        assert!(args[3].contains(&STANDARD.encode("*中文/\"quoted\"*")));
    }
}

#[tokio::test]
async fn agent_shell_images_forward_source_and_validate_bridge_data() {
    use base64::{Engine, engine::general_purpose::STANDARD};

    let png = STANDARD.encode(b"\x89PNG\r\n\x1a\n");
    let (emacs, runner, _) = service(vec![
        conversation_reply(json!({"mime": "image/png", "data": png})),
        conversation_reply(Value::Null),
        conversation_reply(json!({"mime": "image/jpeg", "data": png})),
        conversation_reply(json!({"mime": "image/png", "data": "invalid"})),
        Ok(wire(json!({"result": "nil"}))),
    ]);
    let route = "/agent-shell/sessions/session/image";
    let (status, reply) = request(emacs.clone(), "GET", route, Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reply["error"]["code"], "invalid-request");
    assert!(runner.calls.lock().unwrap().is_empty());

    let source = "https://example.invalid/图 片.png?q=\"w\"";
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("source", source)
        .finish();
    let route = format!("{route}?{query}");
    for (expected, code) in [
        (StatusCode::OK, None),
        (StatusCode::NOT_FOUND, Some("agent-shell-image-not-found")),
        (StatusCode::BAD_GATEWAY, Some("invalid-emacs-response")),
        (StatusCode::BAD_GATEWAY, Some("invalid-emacs-response")),
        (StatusCode::SERVICE_UNAVAILABLE, Some("emacs-unavailable")),
    ] {
        let (status, reply) = request(emacs.clone(), "GET", &route, Value::Null).await;
        assert_eq!(status, expected);
        if let Some(code) = code {
            assert_eq!(reply["error"]["code"], code);
        } else {
            assert_eq!(
                reply,
                json!({"api_version": "1", "mime": "image/png", "data": png})
            );
        }
    }
    for (_, args) in runner.calls.lock().unwrap().iter() {
        assert_conversation_lisp(args);
        assert!(args[3].contains("loki-assistant-agent-shell-read-image name argument"));
        assert!(args[3].contains(&STANDARD.encode(source)));
        assert!(!args[3].contains("example.invalid"));
    }
}

#[tokio::test]
async fn agent_shell_stream_validates_request_before_reading_emacs() {
    let (emacs, runner, _) = service(vec![]);
    let router = routes::router(emacs);
    for (session, expected) in [
        ("%FF", StatusCode::BAD_REQUEST),
        ("%22%29", StatusCode::NOT_ACCEPTABLE),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/agent-shell/sessions/{session}/events"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    for query in [
        "recent=9",
        "recent=abc",
        "recent=10&recent=10",
        "after=-1",
        "after=18446744073709551616",
        "after=5&after=5",
        "unknown=10",
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/agent-shell/sessions/session/events?{query}"))
                    .header("accept", "text/event-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

fn service(
    outputs: Vec<Result<CommandOutput, EmacsError>>,
) -> (Arc<EmacsService>, Arc<FakeRunner>, Arc<ConfigService>) {
    let config = Arc::new(ConfigService::new(test_metadata()));
    let runner = Arc::new(FakeRunner {
        outputs: Mutex::new(outputs.into()),
        ..Default::default()
    });
    (
        Arc::new(EmacsService::with_runner(
            EmacsConfig::new(config.clone()),
            default_notifications(),
            default_status(),
            runner.clone(),
            Duration::from_millis(100),
        )),
        runner,
        config,
    )
}

fn test_metadata() -> crate::modules::config::model::ConfigModuleMetadata {
    let mut metadata = EmacsConfig::metadata();
    metadata.items[0].default = Some(json!("/default/emacsclient"));
    metadata
}

#[tokio::test]
async fn optional_eval_detects_missing_server_without_starting_emacs() {
    let (emacs, runner, _) = service(vec![
        Ok(CommandOutput {
            success: true,
            status: Some(0),
            stdout: String::new(),
            stderr: "emacsclient: can't find socket".into(),
        }),
        Ok(wire(json!({"result": "running"}))),
        Ok(wire(json!({"error": "索引失败"}))),
    ]);

    assert_eq!(emacs.eval_running("t").await.unwrap(), None);
    assert_eq!(
        emacs.eval_running("t").await.unwrap().as_deref(),
        Some("running")
    );
    assert!(matches!(
        emacs.eval_running("t").await,
        Err(EmacsError::Lisp(_))
    ));
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    for (_, args) in calls.iter() {
        assert_eq!(args[0], "--alternate-editor=/usr/bin/true");
        assert_eq!(args[1], "--socket-name=server");
    }
}

fn notification(module_id: &str) -> Notification {
    Notification {
        module_id: module_id.to_owned(),
        title: "任务完成".to_owned(),
        app: Some("emacs".to_owned()),
        body: None,
        buffer: Some("*scratch*".to_owned()),
        payload: json!({"task": 42}),
    }
}

#[tokio::test]
async fn eval_uses_argument_boundary_auto_start_and_latest_config() {
    use crate::modules::config::model::ConfigValueSource;
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir()
        .join(format!("loki-emacs-program-{}", std::process::id()))
        .join("Emacs App");
    std::fs::create_dir_all(&directory).unwrap();
    let executable = directory.join("emacsclient");
    std::fs::write(&executable, "测试文件，不执行").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut startup_output = wire(json!({"result":"3"}));
    startup_output.stdout = format!("Starting Emacs daemon.\n{}\n", startup_output.stdout);
    let (service, runner, config) =
        service(vec![Ok(startup_output), Ok(wire(json!({"result":"nil"})))]);
    let module_config = EmacsConfig::new(Arc::clone(&config));
    let default = module_config.emacsclient_path().await.unwrap();
    assert_eq!(default.value, json!("/default/emacsclient"));
    assert_eq!(default.source, ConfigValueSource::Default);
    assert_eq!(service.eval("(+ 1 2)").await.unwrap(), "3");
    config
        .replace("emacs", "emacsclient_path", json!(executable))
        .await
        .unwrap();
    config
        .replace("emacs", "socket_name", json!("目标 socket"))
        .await
        .unwrap();
    let updated = module_config.emacsclient_path().await.unwrap();
    assert_eq!(updated.value, json!(executable));
    assert_eq!(updated.source, ConfigValueSource::User);
    let script = "(message \"中文\")\n(list \"\\\"\" \"$(touch /tmp/nope)\")";
    assert_eq!(service.eval(script).await.unwrap(), "nil");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, "/default/emacsclient");
    assert_eq!(calls[1].0, executable.to_str().unwrap());
    assert_eq!(
        &calls[0].1[..3],
        ["--alternate-editor=", "--socket-name=server", "--eval"]
    );
    assert_eq!(
        &calls[1].1[..3],
        ["--alternate-editor=", "--socket-name=目标 socket", "--eval"]
    );
    assert_eq!(calls[1].1.len(), 4);
    assert!(
        calls[1].1[3].contains(&serde_json::to_string(&format!("(progn\n{script}\n)")).unwrap())
    );
    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn distinguishes_lisp_process_and_transport_failures() {
    let (service, _, _) = service(vec![
        Ok(wire(
            json!({"error":"Symbol’s function definition is void: missing"}),
        )),
        Ok(CommandOutput {
            success: false,
            status: Some(7),
            stdout: "stdout".to_owned(),
            stderr: "连接失败".to_owned(),
        }),
        Err(EmacsError::Process("找不到可执行文件".to_owned())),
        Ok(CommandOutput {
            success: true,
            status: Some(0),
            stdout: "invalid".to_owned(),
            stderr: String::new(),
        }),
    ]);
    assert!(
        matches!(service.eval("(missing)").await, Err(EmacsError::Lisp(message)) if message.contains("missing"))
    );
    assert!(
        matches!(service.eval("t").await, Err(EmacsError::CommandFailed { status: Some(7), message }) if message == "连接失败")
    );
    assert!(matches!(
        service.eval("t").await,
        Err(EmacsError::Process(_))
    ));
    assert!(matches!(
        service.eval("t").await,
        Err(EmacsError::InvalidResponse(_))
    ));
}

#[derive(Debug)]
struct StallingRunner;

impl CommandRunner for StallingRunner {
    fn run<'a>(&'a self, _: &'a str, _: &'a [String]) -> ServiceFuture<'a, CommandOutput> {
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn times_out_and_releases_evaluation_lock() {
    let service = EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
        Arc::new(StallingRunner),
        Duration::from_millis(1),
    );
    for _ in 0..2 {
        assert!(matches!(service.eval("t").await, Err(EmacsError::Timeout)));
    }
}

#[tokio::test]
async fn decodes_unicode_buffer_names_null_files_and_boolean_flags() {
    let buffers = json!([{"name":"中文\"\\\n", "file":null, "modified":false, "major_mode":"fundamental-mode"}, {"name":"代码", "file":"/tmp/代码.rs", "modified":true, "major_mode":"rust-mode"}]);
    let printed = serde_json::to_string(&buffers.to_string()).unwrap();
    let (service, _, _) = service(vec![Ok(wire(json!({"result":printed})))]);
    let result = service.buffers().await.unwrap();
    assert_eq!(result[0].name, "中文\"\\\n");
    assert!(result[0].file.is_none());
    assert!(!result[0].modified);
    assert_eq!(result[1].file.as_deref(), Some("/tmp/代码.rs"));
    assert!(result[1].modified);
}

#[tokio::test]
async fn dispatches_only_to_target_and_releases_registry_lock() {
    let (service, _, _) = service(vec![]);
    let weak = Arc::downgrade(&service);
    service
        .register_notification_handler(
            "tasks",
            Arc::new(move |notification: Notification| {
                let service = weak.upgrade().unwrap();
                async move {
                    assert!(service.unregister_notification_handler("tasks").await);
                    Ok(json!({"title":notification.title,"payload":notification.payload}))
                }
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        service.notify(notification("other")).await,
        Err(EmacsError::HandlerNotFound(_))
    ));
    assert!(matches!(
        service
            .register_notification_handler(
                "tasks",
                Arc::new(|_: Notification| async { Ok(Value::Null) })
            )
            .await,
        Err(EmacsError::HandlerAlreadyRegistered(_))
    ));
    let result = service.notify(notification("tasks")).await.unwrap();
    assert_eq!(result["title"], "任务完成");
    assert_eq!(result["payload"]["task"], 42);
    assert!(matches!(
        service.notify(notification("tasks")).await,
        Err(EmacsError::HandlerNotFound(_))
    ));
}

#[tokio::test]
async fn rejects_invalid_input_before_executing_anything() {
    let (service, runner, config) = service(vec![]);
    assert!(matches!(
        service.eval(" \n").await,
        Err(EmacsError::InvalidRequest(_))
    ));
    let mut request = notification("tasks");
    request.title.clear();
    assert!(matches!(
        service.notify(request).await,
        Err(EmacsError::InvalidRequest(_))
    ));
    assert!(
        config
            .replace("emacs", "emacsclient_path", json!(""))
            .await
            .is_err()
    );
    assert!(
        config
            .replace("emacs", "emacsclient_path", json!("bad\u{0}path"))
            .await
            .is_err()
    );
    for value in [json!(""), json!(" \n"), json!("bad\u{0}socket"), json!(42)] {
        assert!(config.replace("emacs", "socket_name", value).await.is_err());
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

async fn request(
    service: Arc<EmacsService>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = routes::router(service)
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn http_preserves_existing_routes_with_versioned_json() {
    let (service, _, _) = service(vec![
        Ok(wire(json!({"result":"3"}))),
        Ok(wire(json!({"result":"\"[]\""}))),
    ]);
    let (status, body) = request(
        service.clone(),
        "POST",
        "/emacs/eval",
        json!({"script":"(+ 1 2)"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"api_version":"1", "result":"3"}));
    let (status, body) = request(service.clone(), "GET", "/emacs/buffers", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["buffers"], json!([]));
    let (status, body) = request(
        service,
        "POST",
        "/emacs/receiver/notification",
        json!({"title":"通知", "buffer":"*scratch*"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["module_id"], "emacs");
    assert_eq!(body["result"]["status"], "unconfirmed");
    assert_eq!(body["result"]["display"], "macos");
    assert_eq!(body["result"]["activation"], "emacs");
    assert!(uuid::Uuid::parse_str(body["result"]["notification_id"].as_str().unwrap()).is_ok());
}

#[tokio::test]
async fn http_maps_errors_to_stable_json_codes() {
    let (service, _, _) = service(vec![Ok(wire(json!({"error":"测试 Lisp 错误"})))]);
    let (status, body) = request(
        service.clone(),
        "POST",
        "/emacs/eval",
        json!({"script":"(error \"测试\")"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "lisp-error");
    let (status, body) =
        request(service.clone(), "POST", "/emacs/eval", json!({"script":42})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["api_version"], "1");
    assert_eq!(body["error"]["code"], "invalid-request");
    let (status, body) = request(
        service,
        "POST",
        "/emacs/receiver/notification",
        serde_json::to_value(notification("missing")).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "notification-handler-not-found");
}

#[tokio::test]
async fn composition_registers_emacs_routes_metadata_and_config() {
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    for path in ["/discover/modules/emacs", "/meta/emacs", "/data/emacs"] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        if path == "/discover/modules/emacs" {
            assert_eq!(body["apis"].as_array().unwrap().len(), 16);
            let conversation = body["apis"]
                .as_array()
                .unwrap()
                .iter()
                .find(|api| api["id"] == "agent-shell-events")
                .unwrap();
            assert_eq!(conversation["method"], "GET");
            assert_eq!(
                conversation["path"],
                "/agent-shell/sessions/{session_id}/events"
            );
            assert_eq!(conversation["access"], "paired");
            assert_eq!(conversation["supports_sse"], true);
            assert_eq!(conversation["sse_events"][0]["event"], "agent-shell");
            for (id, method, path) in [
                (
                    "agent-shell-submit-message",
                    "POST",
                    "/agent-shell/sessions/{session_id}/messages",
                ),
                (
                    "agent-shell-restart",
                    "POST",
                    "/agent-shell/sessions/{session_id}/restart",
                ),
                (
                    "agent-shell-clear-buffer",
                    "POST",
                    "/agent-shell/sessions/{session_id}/clear-buffer",
                ),
                (
                    "agent-shell-read-image",
                    "GET",
                    "/agent-shell/sessions/{session_id}/image",
                ),
            ] {
                let api = body["apis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|api| api["id"] == id)
                    .unwrap();
                assert_eq!(api["method"], method);
                assert_eq!(api["path"], path);
                assert_eq!(api["access"], "paired");
            }
            let open_app = body["apis"]
                .as_array()
                .unwrap()
                .iter()
                .find(|api| api["id"] == "open-app")
                .unwrap();
            assert_eq!(open_app["method"], "POST");
            assert_eq!(open_app["path"], "/emacs/apps/open");
            assert!(open_app["description"].as_str().unwrap().contains("app"));
            let slack = body["apis"]
                .as_array()
                .unwrap()
                .iter()
                .find(|api| api["id"] == "activate-slack")
                .unwrap();
            assert_eq!(slack["path"], "/emacs/slack/activate");
            assert_eq!(slack["access"], "local");
            for (id, path) in [
                ("open-mail", "/emacs/mail/open"),
                ("open-mail-inbox", "/emacs/mail/inbox/open"),
            ] {
                let api = body["apis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|api| api["id"] == id)
                    .unwrap();
                assert_eq!(api["path"], path);
                assert_eq!(api["method"], "POST");
                assert_eq!(api["access"], "local");
            }
            let notification = body["apis"]
                .as_array()
                .unwrap()
                .iter()
                .find(|api| api["id"] == "notification")
                .unwrap();
            assert_eq!(notification["path"], "/emacs/receiver/notification");
        } else if path == "/meta/emacs" {
            assert_eq!(body["items"][0]["key"], "emacsclient_path");
            assert_eq!(body["items"][0]["data_type"], "executable");
            assert_eq!(
                body["items"][0]["executable"]["default_program"],
                "emacsclient"
            );
        }
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/emacs/eval")
                .header("content-type", "application/json")
                .body(Body::from("{\"script\":\"\"}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/emacs/notification")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn notification_forwards_content_without_running_emacs_lisp() {
    let config = Arc::new(ConfigService::new(test_metadata()));
    let runner = Arc::new(FakeRunner::default());
    let launcher = Arc::new(FakeNotificationLauncher::default());
    let service = EmacsService::with_runner(
        EmacsConfig::new(config),
        notifications(launcher.clone()),
        default_status(),
        runner.clone(),
        Duration::from_millis(100),
    );
    let result = service.notify(notification("emacs")).await.unwrap();
    assert_eq!(result["activation"], "emacs");
    assert!(runner.calls.lock().unwrap().is_empty());
    let calls = launcher.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].title, "任务完成");
    assert_eq!(calls[0].app.as_deref(), Some("emacs"));
    let navigation =
        percent_encoding::percent_decode_str(calls[0].navigation_url.as_deref().unwrap())
            .decode_utf8()
            .unwrap();
    assert!(navigation.contains("/emacs/activate?buffer=%2Ascratch%2A"));
}

#[tokio::test]
async fn notification_remains_unconfirmed_when_the_tray_cannot_be_reached() {
    let config = Arc::new(ConfigService::new(test_metadata()));
    let service = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(config),
        notifications(Arc::new(FakeNotificationLauncher {
            error: Some("菜单栏应用不可用".to_owned()),
            ..Default::default()
        })),
        default_status(),
        Arc::new(FakeRunner::default()),
        Duration::from_millis(100),
    ));
    let (status, body) = request(
        service,
        "POST",
        "/emacs/receiver/notification",
        serde_json::to_value(notification("emacs")).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["status"], "unconfirmed");
}

#[tokio::test]
async fn notification_receipts_are_queryable_idempotent_and_terminal() {
    let (service, runner, _) = service(vec![]);
    let (_, sent) = request(
        service.clone(),
        "POST",
        "/emacs/receiver/notification",
        json!({"title":"通知", "buffer":"*scratch*"}),
    )
    .await;
    let id = sent["result"]["notification_id"].as_str().unwrap();
    let path = format!("/emacs/receiver/notification/{id}");
    let receipt_path = format!("{path}/receipt");
    let (status, state) = request(service.clone(), "GET", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state,
        json!({"api_version":"1", "notification_id":id, "status":"unconfirmed"})
    );
    for _ in 0..2 {
        let (status, state) = request(
            service.clone(),
            "POST",
            &receipt_path,
            json!({"status":"submitted"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(state["status"], "confirmed");
    }
    let (status, body) = request(
        service.clone(),
        "POST",
        &receipt_path,
        json!({"status":"failed", "code":"permission-denied", "message":"未授权"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "notification-receipt-conflict");
    let (_, state) = request(service, "GET", &path, Value::Null).await;
    assert_eq!(state["status"], "confirmed");
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_receipt_preserves_details_and_rejects_unknown_or_invalid_results() {
    let (service, _, _) = service(vec![]);
    let sent = service.notify(notification("emacs")).await.unwrap();
    let id = sent["notification_id"].as_str().unwrap();
    let path = format!("/emacs/receiver/notification/{id}/receipt");
    for body in [
        json!({"status":"forwarded"}),
        json!({"status":"failed"}),
        json!({"status":"failed", "code":"", "message":"错误"}),
        json!({"status":"submitted", "code":"ignored"}),
    ] {
        let (status, body) = request(service.clone(), "POST", &path, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["api_version"], "1");
    }
    let receipt = json!({"status":"failed", "code":"permission-denied", "message":"请允许通知"});
    for _ in 0..2 {
        let (status, body) = request(service.clone(), "POST", &path, receipt.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["code"], "permission-denied");
        assert_eq!(body["message"], "请允许通知");
        assert_eq!(body["status"], "unconfirmed");
    }
    let unknown = "/emacs/receiver/notification/550e8400-e29b-41d4-a716-446655440000";
    for (method, path, body) in [
        ("GET", unknown.to_owned(), Value::Null),
        ("POST", format!("{unknown}/receipt"), receipt),
    ] {
        let (status, body) = request(service.clone(), method, &path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "notification-not-found");
    }
    let restarted = EmacsService::new(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
    );
    assert!(matches!(
        restarted.notification_status(id).await,
        Err(EmacsError::SystemNotification(NotificationError::NotFound))
    ));
}

#[derive(Debug, Default)]
struct EarlyReceiptLauncher {
    service: Mutex<std::sync::Weak<EmacsService>>,
}

impl NotificationLauncher for EarlyReceiptLauncher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        let service = self.service.lock().unwrap().upgrade().unwrap();
        Box::pin(async move {
            service
                .receive_notification_receipt(
                    &content.notification_id,
                    super::protocol::NotificationReceipt::Submitted {},
                )
                .await
                .unwrap();
            Ok(())
        })
    }
}

#[tokio::test]
async fn registers_before_forwarding_and_preserves_early_receipt() {
    let launcher = Arc::new(EarlyReceiptLauncher::default());
    let service = Arc::new(EmacsService::new(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        notifications(launcher.clone()),
        default_status(),
    ));
    *launcher.service.lock().unwrap() = Arc::downgrade(&service);
    let result = service.notify(notification("emacs")).await.unwrap();
    let id = result["notification_id"].as_str().unwrap();
    assert_eq!(
        service.notification_status(id).await.unwrap().status,
        notification::protocol::NotificationStatus::Confirmed
    );
}

#[tokio::test]
async fn activation_without_buffer_opens_emacs_through_services() {
    let (service, runner, _) = service(vec![Ok(wire(json!({"result":"t"})))]);
    let (status, body) = request(service, "POST", "/emacs/activate", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "activation":"emacs", "buffer":null})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls[0].0, "/usr/bin/open");
    assert_eq!(calls[0].1, ["-b", "org.gnu.Emacs"]);
}

#[tokio::test]
async fn activation_preserves_buffer_name_and_reports_missing_target() {
    let (service, runner, _) = service(vec![
        Ok(wire(json!({"result":"t"}))),
        Ok(wire(json!({"result":"nil"}))),
    ]);
    let buffer = "中文 buffer \"\\\n";
    let path = format!(
        "/emacs/activate?buffer={}",
        percent_encoding::utf8_percent_encode(buffer, percent_encoding::NON_ALPHANUMERIC)
    );
    let (status, body) = request(service.clone(), "POST", &path, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["buffer"], buffer);
    let (status, body) = request(service.clone(), "POST", &path, Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "buffer-not-found");
    let (status, _) = request(service, "POST", "/emacs/activate?buffer=", Value::Null).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        &calls[0].1[..3],
        [
            "--alternate-editor=/usr/bin/false",
            "--socket-name=server",
            "--eval"
        ]
    );
}

#[tokio::test]
async fn activation_reports_connection_failure_without_retry_or_opening_another_instance() {
    let (service, runner, config) = service(vec![
        Ok(CommandOutput {
            success: false,
            status: Some(1),
            stdout: String::new(),
            stderr: "can't connect to socket".to_owned(),
        }),
        Err(EmacsError::Process("找不到 emacsclient".to_owned())),
        Ok(wire(json!({"result":"unexpected"}))),
    ]);
    config
        .replace("emacs", "socket_name", json!("/tmp/不存在的 Emacs socket"))
        .await
        .unwrap();
    for _ in 0..2 {
        let (status, body) = request(
            service.clone(),
            "POST",
            "/emacs/activate?buffer=old",
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "emacs-unavailable");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("无法连接目标 Emacs")
        );
    }
    let (status, body) = request(service, "POST", "/emacs/activate?buffer=old", Value::Null).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["error"]["code"], "invalid-emacs-response");
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 3);
    for (program, args) in calls.iter() {
        assert_eq!(program, "/default/emacsclient");
        assert_eq!(
            &args[..3],
            [
                "--alternate-editor=/usr/bin/false",
                "--socket-name=/tmp/不存在的 Emacs socket",
                "--eval"
            ]
        );
    }
}

#[tokio::test]
async fn activation_timeout_has_a_clear_error_and_does_not_retry() {
    let service = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
        Arc::new(StallingRunner),
        Duration::from_millis(1),
    ));
    let (status, body) = request(service, "POST", "/emacs/activate?buffer=old", Value::Null).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "emacs-timeout");
}

#[tokio::test]
#[ignore = "需要通过 LOKI_TEST_EMACS_SOCKET 指定独立、无 GUI frame 的测试 daemon"]
async fn activation_in_live_emacs_without_gui() {
    let socket = std::env::var("LOKI_TEST_EMACS_SOCKET").expect("必须指定独立测试 socket");
    assert!(
        socket.contains("loki-activation-test-"),
        "禁止使用日常 Emacs 实例"
    );
    let config = Arc::new(ConfigService::new(EmacsConfig::metadata()));
    config
        .replace("emacs", "socket_name", json!(socket))
        .await
        .unwrap();
    let service = EmacsService::new(
        EmacsConfig::new(config.clone()),
        default_notifications(),
        default_status(),
    );
    assert_eq!(
        service
            .eval("(and (daemonp) (not (seq-some #'display-graphic-p (frame-list))))")
            .await
            .unwrap(),
        "t"
    );
    let pid = service.eval("(emacs-pid)").await.unwrap();
    let buffer = "中文 & # + % \"引号\" \\ \n\t😀";
    let name = serde_json::to_string(buffer).unwrap();
    service
        .eval(&format!(
            r#"(with-current-buffer (get-buffer-create {name})
  (insert "验收内容：中文和引号完整"))"#
        ))
        .await
        .unwrap();

    // 从纯 daemon 创建真实 GUI frame，并核对名称与内容。
    service.activate(Some(buffer)).await.unwrap();
    assert_eq!(
        service
            .eval(&format!(
                r#"(progn
  (setq loki-assistant-test-frame (selected-frame)
        loki-assistant-test-window (get-buffer-window {name} t))
  (and (display-graphic-p loki-assistant-test-frame)
       loki-assistant-test-window
       (eq (window-buffer loki-assistant-test-window) (get-buffer {name}))
       (equal (with-current-buffer {name} (buffer-string)) "验收内容：中文和引号完整")))"#
            ))
            .await
            .unwrap(),
        "t"
    );

    // 另一个 GUI frame 成为当前窗口后，仍回到已经显示目标的窗口。
    service
        .eval(
            r#"(progn
  (select-frame (make-frame '((window-system . ns) (name . "其他测试窗口"))))
  (switch-to-buffer "*scratch*")
  (setq loki-assistant-test-frame-count (length (frame-list))))"#,
        )
        .await
        .unwrap();
    service.activate(Some(buffer)).await.unwrap();
    assert_eq!(
        service
            .eval(
                r#"(and (eq (selected-frame) loki-assistant-test-frame)
     (eq (selected-window) loki-assistant-test-window)
     (= (length (frame-list)) loki-assistant-test-frame-count))"#
            )
            .await
            .unwrap(),
        "t"
    );
    service
        .eval("(iconify-frame loki-assistant-test-frame)")
        .await
        .unwrap();
    service.activate(Some(buffer)).await.unwrap();
    assert_eq!(
        service
            .eval("(eq t (frame-visible-p loki-assistant-test-frame))")
            .await
            .unwrap(),
        "t"
    );
    service
        .eval("(make-frame-invisible loki-assistant-test-frame t)")
        .await
        .unwrap();
    service.activate(Some(buffer)).await.unwrap();
    assert_eq!(
        service
            .eval("(eq t (frame-visible-p loki-assistant-test-frame))")
            .await
            .unwrap(),
        "t"
    );

    service
        .eval(&format!("(kill-buffer {name})"))
        .await
        .unwrap();
    assert!(matches!(
        service.activate(Some(buffer)).await,
        Err(EmacsError::BufferNotFound)
    ));
    assert_eq!(service.eval(&format!("(and (not (get-buffer {name})) (= (length (frame-list)) loki-assistant-test-frame-count))")).await.unwrap(), "t");

    // 默认实例仍运行；不存在的显式 socket 必须失败，不能启动新会话。
    config
        .replace("emacs", "socket_name", json!(format!("{socket}-missing")))
        .await
        .unwrap();
    assert!(matches!(
        service.activate(Some("*scratch*")).await,
        Err(EmacsError::ConnectionFailed(_))
    ));
    config
        .replace("emacs", "socket_name", json!(socket))
        .await
        .unwrap();
    assert_eq!(service.eval("(emacs-pid)").await.unwrap(), pid);
    assert_eq!(
        service
            .eval("(= (length (frame-list)) loki-assistant-test-frame-count)")
            .await
            .unwrap(),
        "t"
    );
}

#[tokio::test]
async fn open_config_app_uses_gateway_and_latest_socket() {
    let (service, runner, config) = service(vec![Ok(wire(json!({"result":"t"})))]);
    config
        .replace("emacs", "socket_name", json!("目标 socket"))
        .await
        .unwrap();
    let (status, body) = request(service, "POST", "/emacs/apps/open?app=config", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "app":"config", "opened":true})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "/default/emacsclient");
    assert_eq!(
        &calls[0].1[..3],
        ["--alternate-editor=", "--socket-name=目标 socket", "--eval"]
    );
    assert!(calls[0].1[3].contains("loki-assistant-config"));
    assert!(calls[0].1[3].contains("select-frame-set-input-focus"));
}

#[tokio::test]
async fn open_logs_app_calls_log_viewer_through_gateway() {
    let (service, runner, _) = service(vec![Ok(wire(json!({"result":"t"})))]);
    let (status, body) = request(service, "POST", "/emacs/apps/open?app=logs", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "app":"logs", "opened":true})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1[3].contains("loki-assistant-log"));
    assert!(calls[0].1[3].contains("loki-assistant-log-open-app"));
}

#[tokio::test]
async fn activates_slack_through_emacs_dispatch() {
    let (service, runner, _) = service(vec![Ok(wire(json!({"result":"t"})))]);
    let (status, body) = request(service, "POST", "/emacs/slack/activate", Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1", "app":"slack", "opened":true})
    );
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].1[3].contains("loki-assistant-dispatch-activate-slack"));
}

#[tokio::test]
async fn opens_mail_and_configured_inbox_through_emacs() {
    let (service, runner, _) = service(vec![
        Ok(wire(json!({"result":"t"}))),
        Ok(wire(json!({"result":"t"}))),
        Ok(wire(json!({"result":"t"}))),
        Ok(wire(json!({"result":"t"}))),
    ]);
    for path in [
        "/emacs/mail/open?message_id=a%2Bb%40example.org",
        "/emacs/mail/inbox/open",
        "/mail/open?message_id=a%2Bb%40example.org",
        "/mail/unread/open",
    ] {
        let (status, body) = request(service.clone(), "POST", path, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(body, json!({"api_version":"1", "opened":true}));
    }
    let (status, body) = request(
        service,
        "POST",
        "/emacs/mail/open?message_id=%0A",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid-request");

    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    for pair in calls.chunks_exact(2) {
        assert!(
            pair[0].1[3]
                .contains(r#"loki-assistant-mail-dispatch \"view\" \"ImErYkBleGFtcGxlLm9yZyI=\""#)
        );
        assert!(pair[1].1[3].contains(r#"loki-assistant-mail-dispatch \"inbox\" \"bnVsbA==\""#));
    }
}

#[tokio::test]
async fn open_app_rejects_invalid_and_unknown_identifiers_without_execution() {
    let (service, runner, _) = service(vec![]);
    for (path, expected, code) in [
        (
            "/emacs/apps/open",
            StatusCode::BAD_REQUEST,
            "invalid-request",
        ),
        (
            "/emacs/apps/open?app=",
            StatusCode::BAD_REQUEST,
            "invalid-request",
        ),
        (
            "/emacs/apps/open?app=%20",
            StatusCode::BAD_REQUEST,
            "invalid-request",
        ),
        (
            "/emacs/apps/open?app=missing",
            StatusCode::NOT_FOUND,
            "app-not-found",
        ),
        (
            "/emacs/apps/open?app=%28kill-emacs%29",
            StatusCode::NOT_FOUND,
            "app-not-found",
        ),
    ] {
        let (status, body) = request(service.clone(), "POST", path, Value::Null).await;
        assert_eq!(status, expected);
        assert_eq!(body["api_version"], "1");
        assert_eq!(body["error"]["code"], code);
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    }
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn open_app_preserves_missing_package_lisp_and_connection_failures() {
    let (service, runner, _) = service(vec![
        Ok(wire(json!({"result":"nil"}))),
        Ok(wire(json!({"error":"配置界面加载失败"}))),
        Err(EmacsError::Process("无法启动 emacsclient".to_owned())),
        Ok(wire(json!({"result":"unexpected"}))),
    ]);
    for (expected, code) in [
        (StatusCode::NOT_FOUND, "app-not-found"),
        (StatusCode::UNPROCESSABLE_ENTITY, "lisp-error"),
        (StatusCode::SERVICE_UNAVAILABLE, "emacs-unavailable"),
        (StatusCode::BAD_GATEWAY, "invalid-emacs-response"),
    ] {
        let (status, body) = request(
            service.clone(),
            "POST",
            "/emacs/apps/open?app=config",
            Value::Null,
        )
        .await;
        assert_eq!(status, expected);
        assert_eq!(body["error"]["code"], code);
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    }
    assert_eq!(runner.calls.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn open_app_reports_timeout() {
    let service = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        default_status(),
        Arc::new(StallingRunner),
        Duration::from_millis(1),
    ));
    let (status, body) = request(service, "POST", "/emacs/apps/open?app=config", Value::Null).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(body["error"]["code"], "emacs-timeout");
}

#[tokio::test]
async fn buffer_activation_resolves_name_in_emacs_without_starting_a_daemon() {
    let (service, runner, _) = service(vec![
        Ok(wire(json!({"result":"t"}))),
        Ok(wire(json!({"result":"nil"}))),
    ]);
    let (status, body) = request(
        service.clone(),
        "POST",
        "/emacs/activate?buffer=opaque%22name",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["buffer"], "opaque\"name");
    let (status, body) = request(
        service.clone(),
        "POST",
        "/emacs/activate?buffer=gone",
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "buffer-not-found");
    for path in ["/emacs/activate?buffer="] {
        assert_eq!(
            request(service.clone(), "POST", path, Value::Null).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1[0], "--alternate-editor=/usr/bin/false");
    assert!(calls[0].1[3].contains("get-buffer"));
    assert!(calls[0].1[3].contains("select-frame-set-input-focus"));
}

#[tokio::test]
async fn receiver_status_publishes_through_the_injected_rust_status_service() {
    let status = default_status();
    status
        .publish("network", "my", json!({"status":"disconnected"}))
        .await
        .unwrap();
    let runner = Arc::new(FakeRunner::default());
    let service = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        status.clone(),
        runner.clone(),
        Duration::from_secs(1),
    ));
    let sessions = json!([
        {"id":"session-1", "title":"整理会话列表", "phase":"running"},
        {"id":"session-2", "phase":"completed", "stop_reason":"end_turn"}
    ]);
    let values = json!({"emacs":{
        "runtime":{"pid":42,"version":"31.1","started_at":1,"reported_at":2},
        "agent_shell":sessions
    }});
    let (response, body) = request(service.clone(), "POST", "/emacs/receiver/status", values).await;
    assert_eq!(response, StatusCode::OK);
    assert_eq!(body["status"]["network"]["my"]["status"], "disconnected");
    assert_eq!(body["status"]["emacs"]["agent_shell"], sessions);
    assert_eq!(
        status.value("emacs", "runtime").await.unwrap().value,
        json!({"pid":42,"version":"31.1","started_at":1,"reported_at":2})
    );
    let before = serde_json::to_value(status.snapshot().await).unwrap();
    for values in [
        json!([]),
        json!(null),
        json!({}),
        json!({"emacs":[]}),
        json!({"emacs":{"":1}}),
        json!({"ok":{"value":2},"bad/module":{"value":1}}),
        json!({"emacs":{"agent_shell":[{"id":"session-1", "phase":"busy"}]}}),
        json!({"emacs":{"agent_shell":[{"id":"session-1", "title":7, "phase":"running"}]}}),
        json!({"emacs":{"agent_shell":[{"id":"session-1", "title":null, "phase":"running"}]}}),
        json!({"emacs":{"agent_shell":[{"id":"session-1", "phase":"running", "prompt":"私有内容"}]}}),
        json!({"emacs":{"agent_shell":[{"id":"session-1", "phase":"failed", "stop_reason":"其他说明"}]}}),
    ] {
        let (code, body) = request(service.clone(), "POST", "/emacs/receiver/status", values).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-request");
        assert_eq!(
            serde_json::to_value(status.snapshot().await).unwrap(),
            before
        );
    }
    let (response, body) = request(
        service,
        "POST",
        "/emacs/receiver/status",
        json!({"emacs":{"agent_shell":[]}}),
    )
    .await;
    assert_eq!(response, StatusCode::OK);
    assert_eq!(body["status"]["emacs"]["agent_shell"], json!([]));
    assert_eq!(body["status"]["emacs"]["runtime"]["pid"], 42);
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn external_status_schema_errors_return_paths_and_preserve_the_entire_snapshot() {
    let status = default_status();
    status
        .publish("network", "my", json!({"status":"disconnected"}))
        .await
        .unwrap();
    let runner = Arc::new(FakeRunner::default());
    let service = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(test_metadata()))),
        default_notifications(),
        status.clone(),
        runner.clone(),
        Duration::from_secs(1),
    ));
    let runtime = json!({"pid":42,"version":"31.1","started_at":1,"reported_at":2});
    let (code, _) = request(
        service.clone(),
        "POST",
        "/emacs/receiver/status",
        json!({"emacs":{"runtime":runtime.clone()}}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let before = serde_json::to_value(status.snapshot().await).unwrap();
    let mut extra_field = runtime.clone();
    extra_field["unexpected"] = json!("DO_NOT_ECHO_VALUE");
    let mut wrong_pid = runtime.clone();
    wrong_pid["pid"] = json!(42.5);
    let mut wrong_version = runtime;
    wrong_version["version"] = json!(31);
    for (values, expected) in [
        (json!({"emacs":{"buffers":[]}}), "emacs/buffers"),
        (json!({"emacs":{"runtime":null}}), "emacs/runtime"),
        (
            json!({"emacs":{"runtime":{"pid":42}}}),
            "emacs/runtime.version",
        ),
        (
            json!({"emacs":{"runtime":extra_field}}),
            "emacs/runtime.unexpected",
        ),
        (json!({"emacs":{"runtime":wrong_pid}}), "emacs/runtime.pid"),
        (
            json!({"emacs":{"runtime":wrong_version}}),
            "emacs/runtime.version",
        ),
        (
            json!({"network":{"my":{"status":"DO_NOT_ECHO_VALUE"}}}),
            "network/my.status",
        ),
        (json!({"unknown":{"value":1}}), "unknown/value"),
        (json!({"emacs":{"unknown":[]}}), "emacs/unknown"),
        (
            json!({"emacs":{"runtime":{"pid":43,"version":"31.1","started_at":1,"reported_at":3}},"network":{"my":{"status":"bad"}}}),
            "network/my.status",
        ),
    ] {
        let (code, body) = request(service.clone(), "POST", "/emacs/receiver/status", values).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body["api_version"], "1");
        assert_eq!(body["error"]["code"], "invalid-request");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{body}"
        );
        assert!(!body.to_string().contains("DO_NOT_ECHO_VALUE"));
        assert_eq!(
            serde_json::to_value(status.snapshot().await).unwrap(),
            before
        );
    }
    for value in [
        json!("DO_NOT_ECHO_VALUE"),
        json!({"emacs":"DO_NOT_ECHO_VALUE"}),
    ] {
        let (code, body) = request(service.clone(), "POST", "/emacs/receiver/status", value).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert!(!body.to_string().contains("DO_NOT_ECHO_VALUE"));
        assert_eq!(
            serde_json::to_value(status.snapshot().await).unwrap(),
            before
        );
    }
    assert!(runner.calls.lock().unwrap().is_empty());
    let (code, _) = request(
        service,
        "POST",
        "/emacs/receiver/status",
        json!({"emacs":{"runtime":{"pid":43,"version":"31.1","started_at":1,"reported_at":3}}}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(
        status.value("emacs", "runtime").await.unwrap().value["pid"],
        43
    );
}
