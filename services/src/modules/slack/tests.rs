use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Form, State},
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

use crate::{
    modules::{config::service::ConfigService, notification::NotificationService},
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

use super::{
    protocol::Error,
    routes,
    service::{SlackService, now},
    store::{Keychain, Secrets},
};

#[derive(Default)]
struct MemoryKeychain(Mutex<Secrets>);

impl Keychain for MemoryKeychain {
    fn load(&self) -> Result<Secrets, Error> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, secrets: &Secrets) -> Result<(), Error> {
        *self.0.lock().unwrap() = secrets.clone();
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Alerts(Mutex<Vec<NotificationContent>>);

impl NotificationLauncher for Alerts {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        self.0.lock().unwrap().push(content);
        Box::pin(async { Ok(()) })
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("loki-slack-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Mock {
    socket: String,
    root: String,
    reply: String,
    sent: String,
    challenge: Mutex<String>,
    posts: AtomicUsize,
}

async fn invoke(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    session: Option<Uuid>,
    idempotency_key: Option<Uuid>,
    payload: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:47321");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(session) = session {
        request = request.header("x-loki-slack-session", session.to_string());
    }
    if let Some(key) = idempotency_key {
        request = request.header("idempotency-key", key.to_string());
    }
    let body = if let Some(payload) = payload {
        request = request.header("content-type", "application/json");
        Body::from(payload.to_string())
    } else {
        Body::empty()
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, 1_048_576).await.unwrap();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (parts.status, parts.headers, value)
}

#[tokio::test]
async fn pkce_socket_notifications_threads_and_idempotent_sends() {
    let directory = Directory::new();
    let sockets = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let web = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", web.local_addr().unwrap());
    let now = now() + 5;
    let mock = Arc::new(Mock {
        socket: format!("ws://{}", sockets.local_addr().unwrap()),
        root: format!("{now}.000001"),
        reply: format!("{now}.000002"),
        sent: format!("{now}.000003"),
        challenge: Mutex::default(),
        posts: AtomicUsize::default(),
    });
    let slack = Router::new()
        .route("/apps.connections.open", post(|State(mock): State<Arc<Mock>>| async move {
            Json(json!({"ok":true,"url":mock.socket}))
        }))
        .route("/oauth.v2.access", post(|State(mock): State<Arc<Mock>>, Form(form): Form<std::collections::HashMap<String, String>>| async move {
            assert_eq!(form["redirect_uri"], "http://localhost:47321/slack/oauth/callback");
            assert_eq!(form["code"], "temporary");
            assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())), *mock.challenge.lock().unwrap());
            Json(json!({"ok":true,"authed_user":{"access_token":"fake-user-token"}}))
        }))
        .route("/auth.test", post(|| async { Json(json!({"ok":true,"team_id":"TTEAM","user_id":"UME","team":"工作区"})) }))
        .route("/users.conversations", get(|| async {
            Json(json!({"ok":true,"channels":[{"id":"DDIRECT","user":"UYOU","is_im":true}]}))
        }))
        .route("/users.info", get(|| async { Json(json!({"ok":true,"user":{"real_name":"林舟"}})) }))
        .route("/conversations.history", get(|State(mock): State<Arc<Mock>>| async move {
            Json(json!({"ok":true,"messages":[{"ts":mock.root,"text":"问候","user":"UYOU","reply_count":1}]}))
        }))
        .route("/conversations.replies", get(|State(mock): State<Arc<Mock>>| async move {
            Json(json!({"ok":true,"messages":[{"ts":mock.root,"thread_ts":mock.root,"text":"问候","user":"UYOU"},
                  {"ts":mock.reply,"thread_ts":mock.root,"text":"线程回复","user":"UYOU"}]}))
        }))
        .route("/chat.postMessage", post(|State(mock): State<Arc<Mock>>, headers: HeaderMap, Json(body): Json<Value>| async move {
            assert_eq!(headers["authorization"], "Bearer fake-user-token");
            assert_eq!(body["thread_ts"], mock.root);
            assert_eq!(body["text"], "收到");
            mock.posts.fetch_add(1, Ordering::SeqCst);
            Json(json!({"ok":true,"ts":mock.sent,"message":{"ts":mock.sent,"thread_ts":mock.root,
                        "text":"收到","user":"UME"}}))
        })).with_state(Arc::clone(&mock));
    let http = tokio::spawn(async {
        axum::serve(web, slack).await.unwrap();
    });
    let (done, acked) = tokio::sync::oneshot::channel();
    let socket_mock = Arc::clone(&mock);
    let socket = tokio::spawn(async move {
        let mut stream = accept_async(sockets.accept().await.unwrap().0)
            .await
            .unwrap();
        stream
            .send(Message::Text(r#"{"type":"hello"}"#.into()))
            .await
            .unwrap();
        for envelope in ["first", "retry"] {
            stream
                .send(Message::Text(
                    json!({"type":"events_api","envelope_id":envelope,"payload":{
                "team_id":"TTEAM", "event_id":"EvDuplicate",
                "authorizations":[{"user_id":"UME","is_bot":false}],"event":{
                    "type":"message","channel_type":"im","channel":"DDIRECT","ts":socket_mock.root,
                    "user":"UYOU","text":"问候"}}})
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let answer = stream.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&answer).unwrap()["envelope_id"],
                envelope
            );
        }
        let _ = done.send(());
        std::future::pending::<()>().await;
    });

    let keychain = Arc::new(MemoryKeychain::default());
    let alerts = Arc::new(Alerts::default());
    let notification = Arc::new(NotificationService::with_launcher(
        alerts.clone(),
        Duration::from_secs(1),
    ));
    let service_path = directory.0.join("service");
    let service = Arc::new(SlackService::with_dependencies(
        Some("123.456"),
        notification.clone(),
        keychain.clone(),
        &api,
        Some(service_path.clone()),
    ));
    let worker = tokio::spawn(Arc::clone(&service).run(47321));
    timeout(Duration::from_secs(5), async {
        loop {
            if service.state.lock().await.initialized {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let token = fs::read_to_string(directory.0.join("slack-api-token")).unwrap();
    let app = routes::router(service.clone());
    let (status, headers, _) = invoke(&app, "GET", "/slack/status", None, None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["cache-control"], "no-store");
    let auth = Some(token.as_str());
    service.app_token("xapp-test".into()).await.unwrap();
    let (status, headers, value) = invoke(
        &app,
        "POST",
        "/slack/oauth/authorizations",
        auth,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let authorization = headers["location"].to_str().unwrap().to_owned();
    let url = Url::parse(value["authorization_url"].as_str().unwrap()).unwrap();
    let parameters: std::collections::HashMap<_, _> = url.query_pairs().collect();
    *mock.challenge.lock().unwrap() = parameters["code_challenge"].to_string();
    let callback = format!(
        "/slack/oauth/callback?code=temporary&state={}",
        parameters["state"]
    );
    assert_eq!(
        invoke(&app, "GET", &callback, None, None, None, None)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        invoke(&app, "GET", &callback, None, None, None, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        invoke(&app, "GET", &authorization, auth, None, None, None)
            .await
            .2["state"],
        "succeeded"
    );
    let data = invoke(&app, "GET", "/slack/status", auth, None, None, None)
        .await
        .2;
    let session = Some(Uuid::parse_str(data["session"]["session_id"].as_str().unwrap()).unwrap());
    timeout(Duration::from_secs(5), acked)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while alerts.0.lock().unwrap().is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(alerts.0.lock().unwrap().len(), 1);

    let timeline = "/slack/conversations/DDIRECT/messages";
    let thread = format!(
        "/slack/conversations/DDIRECT/threads/{}/messages",
        mock.root
    );
    let read = format!(
        "/slack/conversations/DDIRECT/threads/{}/read-position",
        mock.root
    );
    let (_, _, view) = invoke(&app, "GET", timeline, auth, session, None, None).await;
    assert_eq!(view["items"][0]["ts"], mock.root);
    let (_, _, view) = invoke(&app, "GET", &thread, auth, session, None, None).await;
    assert_eq!(view["root"]["ts"], mock.root);
    assert_eq!(view["items"][0]["ts"], mock.reply);
    let (_, _, view) = invoke(
        &app,
        "PUT",
        &read,
        auth,
        session,
        None,
        Some(json!({"through_ts":mock.reply})),
    )
    .await;
    assert_eq!(view["unread_count"], 1);
    let (_, _, view) = invoke(
        &app,
        "GET",
        "/slack/conversations?unread=true",
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(view["items"][0]["unread_count"], 1);

    let key = Some(Uuid::new_v4());
    let body = json!({"text":"收到"});
    let (first, second) = tokio::join!(
        invoke(
            &app,
            "POST",
            &thread,
            auth,
            session,
            key,
            Some(body.clone())
        ),
        invoke(
            &app,
            "POST",
            &thread,
            auth,
            session,
            key,
            Some(body.clone())
        )
    );
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert!(matches!(second.0, StatusCode::ACCEPTED | StatusCode::OK));
    assert_eq!(first.1["location"], second.1["location"]);
    let send = first.1["location"].to_str().unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if invoke(&app, "GET", send, auth, session, None, None).await.2["state"] == "sent" {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(
        invoke(
            &app,
            "POST",
            &thread,
            auth,
            session,
            key,
            Some(json!({"text":"不同的回复"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    worker.abort();
    let _ = worker.await;

    let restarted = Arc::new(SlackService::with_dependencies(
        Some("123.456"),
        notification,
        keychain,
        &api,
        Some(service_path),
    ));
    restarted.initialize().await.unwrap();
    let app = routes::router(restarted);
    assert_eq!(
        invoke(&app, "GET", send, auth, session, None, None).await.2["state"],
        "sent"
    );
    assert_eq!(
        invoke(&app, "DELETE", "/slack/session", auth, session, None, None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        invoke(&app, "GET", timeline, auth, session, None, None)
            .await
            .0,
        StatusCode::CONFLICT
    );
    socket.abort();
    http.abort();
}

#[derive(Debug)]
struct FakePass;

impl crate::modules::password::service::CommandRunner for FakePass {
    fn run<'a>(
        &'a self,
        _program: &'a str,
        store: &'a std::path::Path,
        args: &'a [String],
    ) -> crate::modules::password::service::CommandFuture<'a> {
        Box::pin(async move {
            assert_eq!(&args[..2], ["show", "--"]);
            let stdout = fs::read(store.join(format!("{}.gpg", args[2]))).unwrap();
            Ok(crate::modules::password::service::CommandOutput {
                status: Some(0),
                stdout,
            })
        })
    }
}

fn fake_pass_write<'a>(
    _program: &'a str,
    store: &'a std::path::Path,
    args: &'a [String],
    input: &'a str,
) -> crate::modules::password::service::CommandFuture<'a> {
    Box::pin(async move {
        assert_eq!(&args[..4], ["insert", "--force", "--multiline", "--"]);
        let path = store.join(format!("{}.gpg", args[4]));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, input).unwrap();
        Ok(crate::modules::password::service::CommandOutput {
            status: Some(0),
            stdout: Vec::new(),
        })
    })
}

#[tokio::test]
async fn project_workflow_uses_metadata_isolated_passwords_and_team_validation() {
    use super::projects::{Backend, Projects};
    use crate::modules::{
        config,
        password::{config::PasswordConfig, service::PasswordService},
        status::StatusService,
    };

    let directory = Directory::new();
    let config_path = directory.0.join("config.yml");
    let base = Arc::new(ConfigService::load(&config_path).unwrap());
    base.replace(
        "password",
        "store_dir",
        json!(directory.0.to_str().unwrap()),
    )
    .await
    .unwrap();
    base.replace("password", "pass_path", json!("/bin/echo"))
        .await
        .unwrap();
    let passwords = Arc::new(PasswordService::with_runner(
        PasswordConfig::new(base.clone()),
        Arc::new(FakePass),
        Duration::from_secs(2),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let slack = Router::new()
        .route(
            "/apps.connections.open",
            post(|| async { Json(json!({"ok":true,"url":"ws://127.0.0.1:1"})) }),
        )
        .route(
            "/oauth.v2.access",
            post(
                |Form(form): Form<std::collections::HashMap<String, String>>| async move {
                    Json(json!({"ok":true,"authed_user":{"access_token":form["code"]}}))
                },
            ),
        )
        .route(
            "/auth.test",
            post(|headers: HeaderMap| async move {
                let team = headers["authorization"]
                    .to_str()
                    .unwrap()
                    .strip_prefix("Bearer ")
                    .unwrap();
                Json(json!({"ok":true,"team_id":team,"user_id":"U1"}))
            }),
        );
    let upstream = tokio::spawn(async move {
        axum::serve(listener, slack).await.unwrap();
    });
    let status = Arc::new(StatusService::new());
    let keychains = Arc::new(Mutex::new(std::collections::BTreeMap::<
        String,
        Arc<MemoryKeychain>,
    >::new()));
    let (browser, mut opened) = tokio::sync::mpsc::unbounded_channel::<String>();
    let projects = Arc::new(Projects::with_backend(
        base.clone(),
        Arc::new(NotificationService::new()),
        status.clone(),
        Backend {
            path: Some(directory.0.join("service")),
            api,
            client_id: Some("123.456"),
            keychain: Arc::new(move |id| {
                keychains
                    .lock()
                    .unwrap()
                    .entry(id)
                    .or_insert_with(|| Arc::new(MemoryKeychain::default()))
                    .clone()
            }),
            passwords: Some(Arc::new(super::password::SlackPasswords::with_writer(
                PasswordConfig::new(base.clone()),
                passwords.clone(),
                fake_pass_write,
            ))),
            emacs: None,
            open_browser: Arc::new(move |url| {
                let browser = browser.clone();
                Box::pin(async move {
                    let fail = Url::parse(&url)
                        .unwrap()
                        .query_pairs()
                        .any(|(key, value)| key == "team" && value == "TCANCEL");
                    browser.send(url).unwrap();
                    if fail {
                        Err("无法打开测试浏览器。".into())
                    } else {
                        Ok(())
                    }
                })
            }),
        },
    ));
    let config = config::Module::from_service(
        (*base)
            .clone()
            .with_change_handler("slack", projects.clone()),
    );
    assert!(status.metadata().iter().any(|module| {
        module.id == "slack"
            && module
                .items
                .iter()
                .any(|item| item.key == "projects" && !item.remote_access)
    }));
    let app = Router::new()
        .merge(config.router())
        .merge(projects.router());
    let worker = tokio::spawn(Arc::clone(&projects).run(47321));
    let token = timeout(Duration::from_secs(4), async {
        loop {
            if let Ok(token) = fs::read_to_string(directory.0.join("slack-api-token")) {
                break token;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let auth = Some(token.as_str());
    timeout(Duration::from_secs(4), async {
        while invoke(
            &app,
            "POST",
            "/config/actions/slack/authorize/TPROBE",
            auth,
            None,
            None,
            None,
        )
        .await
        .0 == StatusCode::UNAUTHORIZED
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let meta = invoke(&app, "GET", "/meta", None, None, None, None).await.2;
    assert!(meta.get("workflows").is_none());
    let module = meta["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == "slack")
        .unwrap();
    let teams = module["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == "teams")
        .unwrap();
    assert!(teams.get("create_action").is_none());
    assert_eq!(teams["save_timeout_seconds"], 650);
    assert_eq!(teams["list"]["item_type"], "structure");
    assert_eq!(teams["structure"]["type_name"], "SlackTeam");
    assert_eq!(
        teams["structure"]["fields"][0]["checker"]["pattern"],
        "^T[A-Z0-9]{1,47}$"
    );

    assert_eq!(teams["actions"][0]["interaction"], "request");
    assert_eq!(teams["actions"][1]["binding"]["key"], "app_token");
    assert_eq!(teams["actions"][2]["id"], "authorize");
    let create = |team: &str, body: Value| {
        Request::builder()
            .method("PUT")
            .uri(format!("/data/slack/teams/{team}"))
            .header("if-none-match", "*")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    for (team, query) in [
        ("TCANCEL", "cancel"),
        ("TDENIED", "error=access_denied"),
        ("TERROR", "code=TWRONG"),
        ("TALPHA", "code=TALPHA"),
        ("TBETA", "code=TBETA"),
    ] {
        let request = create(team, json!({"name":team}));
        let client = app.clone();
        let save = tokio::spawn(async move { client.oneshot(request).await.unwrap() });
        let url = Url::parse(
            &timeout(Duration::from_secs(2), opened.recv())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(params["team"], team);
        let data = invoke(&app, "GET", "/data/slack/teams", None, None, None, None)
            .await
            .2;
        assert!(!data.to_string().contains(team));
        if query == "cancel" {
            let response = timeout(Duration::from_secs(2), save)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let callback = format!(
                "/slack/oauth/callback?code={team}&state={}",
                params["state"]
            );
            assert_eq!(
                invoke(&app, "GET", &callback, None, None, None, None)
                    .await
                    .0,
                StatusCode::BAD_REQUEST
            );
            continue;
        }
        let callback = format!("/slack/oauth/callback?{query}&state={}", params["state"]);
        assert_eq!(
            invoke(&app, "GET", &callback, None, None, None, None)
                .await
                .0,
            StatusCode::SEE_OTHER
        );
        let response = timeout(Duration::from_secs(2), save)
            .await
            .unwrap()
            .unwrap();
        let (parts, body) = response.into_parts();
        let outcome: Value =
            serde_json::from_slice(&to_bytes(body, 1_048_576).await.unwrap()).unwrap();
        assert!(!parts.headers.contains_key("location"));
        assert!(!outcome.to_string().contains("authorization_url"));
        let data = invoke(&app, "GET", "/data/slack/teams", None, None, None, None)
            .await
            .2;
        if matches!(team, "TALPHA" | "TBETA") {
            assert_eq!(parts.status, StatusCode::OK, "{outcome:?}");
            assert_eq!(outcome["value"]["oauth_credential_id"], team);
            assert!(data.to_string().contains(team));
            let duplicate = app
                .clone()
                .oneshot(create(team, json!({"name":"duplicate"})))
                .await
                .unwrap();
            assert_eq!(duplicate.status(), StatusCode::PRECONDITION_FAILED);
        } else {
            assert_eq!(parts.status, StatusCode::INTERNAL_SERVER_ERROR);
            assert!(!data.to_string().contains(team));
        }
    }
    let duplicate_password = app
        .clone()
        .oneshot(create(
            "TTHIRD",
            json!({"name":"第三工作区",
        "password_id":"loki-assistant/slack/TALPHA/app-token.gpg"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        duplicate_password.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let invalid = invoke(
        &app,
        "POST",
        "/data/slack/teams",
        None,
        None,
        None,
        Some(json!([{"team_id":"wrong","name":"check"}])),
    )
    .await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    assert!(invalid.2.to_string().contains("请输入以 T 开头"));
    let check = "/config/actions/slack/check_app_token/TBETA";
    assert_eq!(
        invoke(&app, "POST", check, None, None, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        !invoke(&app, "POST", check, auth, None, None, None)
            .await
            .0
            .is_success()
    );
    let beta_entry = directory.0.join("loki-assistant/slack/TBETA/app-token.gpg");
    fs::create_dir_all(beta_entry.parent().unwrap()).unwrap();
    fs::write(&beta_entry, "xapp-beta\n").unwrap();
    let (check_status, headers, checked) =
        invoke(&app, "POST", check, auth, None, None, None).await;
    assert_eq!(check_status, StatusCode::OK, "{checked:?}");
    assert_eq!(checked["valid"], true);
    assert!(!headers.contains_key("location"));
    assert!(!checked.to_string().contains("xapp-beta"));
    assert_eq!(fs::read_to_string(beta_entry).unwrap(), "xapp-beta\n");
    let payload = json!({"project":"TALPHA","token":"xapp-alpha"});
    let path = "/data/slack/app_token";
    assert_eq!(
        invoke(&app, "PUT", path, None, None, None, Some(payload.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, _, validator) = invoke(
        &app,
        "POST",
        "/config/validate",
        auth,
        None,
        None,
        Some(json!({"module_id":"slack","key":"app_token","value":payload})),
    )
    .await;
    assert_eq!(validator["value"], true);
    assert!(!validator.to_string().contains("xapp-alpha"));
    let (code, _, saved) = invoke(&app, "PUT", path, auth, None, None, Some(payload)).await;
    assert_eq!(code, StatusCode::OK, "{saved:?}");
    assert_eq!(saved["value"], true);
    let config_text = fs::read_to_string(config_path).unwrap();
    assert!(!config_text.contains("xapp-alpha"));
    assert!(!config_text.contains("app_token:"));
    assert_eq!(
        passwords
            .fetch("loki-assistant/slack/TALPHA/app-token.gpg")
            .await
            .unwrap(),
        "xapp-alpha\n"
    );
    let public = status.value("slack", "projects").unwrap().value;
    assert_eq!(public[0]["id"], "TALPHA");
    assert_eq!(public[0]["app_token_set"], true);
    assert_eq!(public[1]["id"], "TBETA");
    assert_eq!(public[1]["app_token_set"], true);
    assert!(!public.to_string().contains("xapp-alpha"));

    let action = "/config/actions/slack/authorize/TALPHA";
    let client = app.clone();
    let secret = auth.unwrap().to_owned();
    let reauthorize = tokio::spawn(async move {
        invoke(&client, "POST", action, Some(&secret), None, None, None).await
    });
    let url = Url::parse(
        &timeout(Duration::from_secs(2), opened.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(params["team"], "TALPHA");
    let callback = format!(
        "/slack/oauth/callback?code=TWRONG&state={}",
        params["state"]
    );
    assert_eq!(
        invoke(&app, "GET", &callback, None, None, None, None)
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    let (status_code, _, outcome) = timeout(Duration::from_secs(2), reauthorize)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status_code, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Slack Team")
    );
    projects.stop().await;
    worker.await.unwrap();
    upstream.abort();
}

#[tokio::test]
async fn composition_mounts_the_registered_team_entry_and_metadata() {
    let services = Arc::new(crate::Services::build());
    let app = crate::http::router(Arc::new(crate::runtime::RuntimeState::new(services)));
    let (status, _, meta) = invoke(
        &app,
        "GET",
        "/discover/modules/slack",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(meta["apis"].as_array().unwrap().iter().any(|api| {
        api["method"] == "POST"
            && api["path"] == "/slack/teams/{team_id}/open"
            && api["access"] == "local"
    }));
    let (status, _, body) = invoke(
        &app,
        "POST",
        "/slack/teams/TMISSING/open",
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "slack-project-not-found");
}
