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
    extract::{Form, Query, State},
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
    modules::{
        config::service::ConfigService, notification::NotificationService, status::StatusService,
    },
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

use super::{
    projects::{Backend, Projects},
    protocol::Error,
    service::now,
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

async fn ready_service(projects: &Projects, team: &str) -> Arc<super::service::SlackService> {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(service) = projects.service(team).await
                && service.state.lock().await.initialized
            {
                break service;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
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
        .route("/conversations.history", get(|State(mock): State<Arc<Mock>>, Query(query): Query<std::collections::HashMap<String, String>>| async move {
            let root = json!({"ts":mock.root,"text":"问候","user":"UYOU","reply_count":2});
            let response = if query.get("latest") == Some(&mock.root) {
                json!({"ok":true,"messages":[{"ts":"1.000001","text":"更早的顶层消息","user":"UYOU"}]})
            } else if query.get("cursor").is_some_and(|cursor| cursor == "after-broadcast") {
                json!({"ok":true,"has_more":true,"messages":[root]})
            } else if query.get("limit").is_some_and(|limit| limit == "1") {
                json!({"ok":true,"messages":[{"ts":mock.reply,"thread_ts":mock.root,"subtype":"thread_broadcast"}],
                       "response_metadata":{"next_cursor":"after-broadcast"}})
            } else {
                json!({"ok":true,"messages":[root]})
            };
            Json(response)
        }))
        .route("/conversations.replies", get(|State(mock): State<Arc<Mock>>, Query(query): Query<std::collections::HashMap<String, String>>| async move {
            let earlier = format!("{}5", mock.root);
            let response = if query.get("oldest") == Some(&earlier) {
                json!({"ok":true,"messages":[{"ts":mock.reply,"thread_ts":mock.root,"text":"线程回复","user":"UYOU"}]})
            } else {
                json!({"ok":true,"has_more":!query.contains_key("latest"),"messages":[
                    {"ts":mock.root,"thread_ts":mock.root,"text":"问候","user":"UYOU"},
                    {"ts":earlier,"thread_ts":mock.root,"text":"更早的回复","user":"UYOU"}
                ]})
            };
            Json(response)
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
    let config = Arc::new(ConfigService::load(&directory.0.join("config.yml")).unwrap());
    config
        .replace_record(
            "slack".into(),
            "teams".into(),
            "TTEAM".into(),
            json!({"name":"工作区"}),
            true,
            true,
        )
        .await
        .unwrap();
    let status = Arc::new(StatusService::new());
    let start = || {
        let keychain = Arc::clone(&keychain);
        let projects = Arc::new(Projects::with_backend(
            Arc::clone(&config),
            Arc::clone(&notification),
            Arc::clone(&status),
            Backend {
                path: Some(directory.0.join("service")),
                api: api.clone(),
                client_id: Some("123.456"),
                keychain: Arc::new(move |_| keychain.clone()),
                passwords: None,
                emacs: None,
                open_browser: Arc::new(|_| unreachable!()),
            },
        ));
        let app = projects.router().merge(status.router());
        let worker = tokio::spawn(Arc::clone(&projects).run(47321));
        (projects, app, worker)
    };
    let (projects, app, worker) = start();
    let service = ready_service(&projects, "TTEAM").await;
    let token = fs::read_to_string(directory.0.join("slack-api-token")).unwrap();
    let (status_code, headers, _) =
        invoke(&app, "GET", "/slack/conversations", None, None, None, None).await;
    assert_eq!(status_code, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["cache-control"], "no-store");
    let auth = Some(token.as_str());
    service.app_token("xapp-test".into()).await.unwrap();
    let (status, headers, value) = invoke(
        &app,
        "POST",
        "/slack/oauth/authorizations?team_id=TTEAM",
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
    let data = invoke(
        &app,
        "GET",
        "/status/slack/connections",
        None,
        None,
        None,
        None,
    )
    .await
    .2;
    let session = Some(Uuid::parse_str(data["value"][0]["session_id"].as_str().unwrap()).unwrap());
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

    let timeline = "/slack/conversations/TTEAM.DDIRECT/messages";
    let thread = format!(
        "/slack/conversations/TTEAM.DDIRECT/threads/{}/messages",
        mock.root
    );
    let read = format!(
        "/slack/conversations/TTEAM.DDIRECT/threads/{}/read-position",
        mock.root
    );
    let (_, _, view) = invoke(&app, "GET", timeline, auth, session, None, None).await;
    assert_eq!(view["items"][0]["ts"], mock.root);
    let (status, _, view) = invoke(
        &app,
        "GET",
        &format!("{timeline}?limit=1"),
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["items"][0]["ts"], mock.root);
    let timeline_cursor = view["next_cursor"].as_str().unwrap();
    let (_, _, view) = invoke(
        &app,
        "GET",
        &format!("{timeline}?limit=1&cursor={timeline_cursor}"),
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(view["items"][0]["ts"], "1.000001");
    assert!(view["next_cursor"].is_null());
    let (_, _, view) = invoke(
        &app,
        "GET",
        &format!("{thread}?limit=1"),
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(view["root"]["ts"], mock.root);
    assert_eq!(view["root"]["author"]["id"], "TTEAM.UYOU");
    assert_eq!(view["items"][0]["ts"], mock.reply);
    let thread_cursor = view["next_cursor"].as_str().unwrap();
    let (_, _, view) = invoke(
        &app,
        "GET",
        &format!("{thread}?limit=1&cursor={thread_cursor}"),
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(view["root"]["ts"], mock.root);
    assert_eq!(view["items"][0]["text"], "更早的回复");
    assert!(view["next_cursor"].is_null());
    let (status, _, error) = invoke(
        &app,
        "GET",
        &format!("{thread}?cursor={timeline_cursor}"),
        auth,
        session,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "slack-invalid-cursor");
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
        None,
        None,
        None,
    )
    .await;
    assert_eq!(view["items"][0]["unread_count"], 1);

    // 已翻阅的旧线程根消息可能已经被逐出缓存，发送前向 Slack 核对。
    let root = mock.root.clone();
    service
        .update_cache(session.unwrap(), move |cache, _| {
            cache
                .conversations
                .get_mut("DDIRECT")
                .unwrap()
                .messages
                .remove(&root);
            Ok(())
        })
        .await
        .unwrap();
    let reply_as_thread = format!(
        "/slack/conversations/TTEAM.DDIRECT/threads/{}/messages",
        mock.reply
    );
    let (status, _, error) = invoke(
        &app,
        "POST",
        &reply_as_thread,
        auth,
        session,
        Some(Uuid::new_v4()),
        Some(json!({"text":"收到"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "slack-thread-not-found");

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
    projects.stop().await;
    worker.await.unwrap();

    let (projects, app, worker) = start();
    ready_service(&projects, "TTEAM").await;
    assert_eq!(
        invoke(&app, "GET", send, auth, session, None, None).await.2["state"],
        "sent"
    );
    assert_eq!(
        invoke(
            &app,
            "DELETE",
            "/slack/session?team_id=TTEAM",
            auth,
            session,
            None,
            None
        )
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
    projects.stop().await;
    worker.await.unwrap();
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
    use crate::modules::{
        config,
        password::{config::PasswordConfig, service::PasswordService},
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
    let passwords = Arc::new(
        PasswordService::with_runner(
            PasswordConfig::new(base.clone()),
            Arc::new(FakePass),
            Duration::from_secs(2),
        )
        .with_writer(fake_pass_write),
    );
    let base = Arc::new((*base).clone().with_password_service(passwords.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let slack = Router::new()
        .route(
            "/users.list",
            get(|| async {
                Json(json!({"ok":true,"members":[
                    {"id":"U1","profile":{"display_name":"林舟"}},
                    {"id":"UOLD","deleted":true}
                ]}))
            }),
        )
        .route(
            "/users.conversations",
            get(|| async {
                Json(json!({"ok":true,"channels":[
                    {"id":"DSHARED","user":"U1","is_im":true},
                    {"id":"CCHAT","name":"项目讨论","is_channel":true},
                    {"id":"GCHAT","name":"内部讨论","is_channel":true,"is_private":true}
                ]}))
            }),
        )
        .route(
            "/conversations.history",
            get(|headers: HeaderMap| async move {
                let identity = headers["authorization"].to_str().unwrap();
                Json(
                    json!({"ok":true,"messages":[{"ts":"100.000001","text":identity,"user":"U2"}]}),
                )
            }),
        )
        .route(
            "/apps.connections.open",
            post(|| async { Json(json!({"ok":true,"url":"ws://127.0.0.1:1"})) }),
        )
        .route(
            "/auth.test",
            post(|headers: HeaderMap| async move {
                let team = headers["authorization"]
                    .to_str()
                    .unwrap()
                    .strip_prefix("Bearer xoxp-")
                    .unwrap()
                    .split('-')
                    .next()
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
    let stored_keychains = Arc::clone(&keychains);
    let (browser, mut opened) = tokio::sync::mpsc::unbounded_channel::<String>();
    let projects = Arc::new(Projects::with_backend(
        base.clone(),
        Arc::new(NotificationService::new()),
        status.clone(),
        Backend {
            path: Some(directory.0.join("service")),
            api,
            client_id: None,
            keychain: Arc::new(move |id| {
                keychains
                    .lock()
                    .unwrap()
                    .entry(id)
                    .or_insert_with(|| Arc::new(MemoryKeychain::default()))
                    .clone()
            }),
            passwords: Some(passwords.clone()),
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
                .any(|item| item.key == "connections" && !item.remote_access)
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
    assert_eq!(teams["structure"]["id_field"], "team_id");
    assert_eq!(teams["structure"]["fields"][0]["required"], true);
    for request in [
        create("TALPHA", json!({})),
        create("INVALID", json!({"name":"无效"})),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let (status_code, _, error) = invoke(
        &app,
        "PUT",
        "/data/slack/teams/TALPHA",
        auth,
        None,
        None,
        Some(json!({"name":"绕过新增处理"})),
    )
    .await;
    assert_eq!(status_code, StatusCode::CONFLICT);
    assert_eq!(
        error["error"]["code"],
        "config-record-authorization-required"
    );
    assert!(opened.try_recv().is_err());
    for (team, supplied, expected) in [
        ("TCANCEL", None, StatusCode::NOT_FOUND),
        ("TDENIED", Some("invalid"), StatusCode::UNPROCESSABLE_ENTITY),
        ("TERROR", Some("xoxp-TWRONG"), StatusCode::CONFLICT),
        ("TALPHA", Some("xoxp-TALPHA"), StatusCode::NO_CONTENT),
        ("TBETA", Some("xoxp-TBETA"), StatusCode::NO_CONTENT),
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
        assert_eq!(url.path(), "/slack/token-import");
        assert_eq!(url.query(), Some(format!("team={team}").as_str()));
        let attempt = url.fragment().unwrap();
        let payload = json!({"team_id":team, "token":supplied.unwrap_or("xoxp-TCANCEL")});
        if team == "TCANCEL" {
            assert_eq!(
                timeout(Duration::from_secs(2), save)
                    .await
                    .unwrap()
                    .unwrap()
                    .status(),
                StatusCode::INTERNAL_SERVER_ERROR
            );
            assert_eq!(
                invoke(
                    &app,
                    "POST",
                    "/slack/token-import",
                    Some(attempt),
                    None,
                    None,
                    Some(payload)
                )
                .await
                .0,
                StatusCode::NOT_FOUND
            );
            continue;
        }
        let current = invoke(&app, "GET", "/data/slack/teams", None, None, None, None)
            .await
            .2;
        assert!(!current.to_string().contains(team));
        let random_attempt = Uuid::new_v4().to_string();
        assert_eq!(
            invoke(
                &app,
                "POST",
                "/slack/token-import",
                Some(&random_attempt),
                None,
                None,
                Some(payload.clone())
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        let (status, _, _) = invoke(
            &app,
            "POST",
            "/slack/token-import",
            Some(attempt),
            None,
            None,
            Some(payload.clone()),
        )
        .await;
        assert_eq!(status, expected);
        let response = timeout(Duration::from_secs(2), save)
            .await
            .unwrap()
            .unwrap();
        let (parts, body) = response.into_parts();
        let outcome: Value =
            serde_json::from_slice(&to_bytes(body, 1_048_576).await.unwrap()).unwrap();
        let file = format!("loki-assistant/slack/{team}/user-token.gpg");
        let data = invoke(&app, "GET", "/data/slack/teams", None, None, None, None)
            .await
            .2;
        assert!(!data.to_string().contains("xoxp-"));
        assert!(!fs::read_to_string(&config_path).unwrap().contains("xoxp-"));
        assert_eq!(
            invoke(
                &app,
                "POST",
                "/slack/token-import",
                Some(attempt),
                None,
                None,
                Some(payload)
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        if matches!(team, "TALPHA" | "TBETA") {
            assert_eq!(parts.status, StatusCode::OK, "{outcome:?}");
            assert_eq!(outcome["value"]["token"], file);
            assert_eq!(
                passwords.fetch(&file).await.unwrap().trim_end(),
                supplied.unwrap()
            );
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
            assert!(!directory.0.join(file).exists());
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
        auth,
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
    let public = status.value("slack", "connections").unwrap().value;
    assert_eq!(public[0]["team_id"], "TALPHA");
    assert_eq!(public[0]["app_token_set"], true);
    assert_eq!(public[1]["team_id"], "TBETA");
    assert_eq!(public[0]["user_id"], "TALPHA.U1");
    assert_eq!(public[1]["user_id"], "TBETA.U1");
    assert_eq!(public[1]["app_token_set"], true);
    assert!(!public.to_string().contains("xapp-alpha"));

    let (_, _, users) = invoke(&app, "GET", "/slack/users?limit=1", auth, None, None, None).await;
    assert_eq!(users["items"][0]["id"], "TALPHA.U1");
    assert_eq!(users["items"][0]["name"], "林舟");
    let cursor = users["next_cursor"].as_str().unwrap();
    let (_, _, users) = invoke(
        &app,
        "GET",
        &format!("/slack/users?cursor={cursor}"),
        auth,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(users["items"][0]["id"], "TBETA.U1");
    assert_eq!(users["items"].as_array().unwrap().len(), 1);
    assert!(users["next_cursor"].is_null());

    let (_, _, first) = invoke(
        &app,
        "GET",
        "/slack/conversations?limit=1&kind=im",
        auth,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(first["items"][0]["team_id"], "TALPHA");
    assert_eq!(first["items"][0]["id"], "TALPHA.DSHARED");
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, _, second) = invoke(
        &app,
        "GET",
        &format!("/slack/conversations?limit=1&kind=im&cursor={cursor}"),
        auth,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(second["items"][0]["team_id"], "TBETA");
    assert_eq!(second["items"][0]["id"], "TBETA.DSHARED");
    let alpha = Uuid::parse_str(first["items"][0]["session_id"].as_str().unwrap()).unwrap();
    assert_ne!(
        first["items"][0]["session_id"],
        second["items"][0]["session_id"]
    );
    let (_, _, channels) = invoke(
        &app,
        "GET",
        "/slack/conversations?team_id=TBETA&kind=public_channel",
        auth,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(channels["items"][0]["title"], "项目讨论");
    let beta = Uuid::parse_str(channels["items"][0]["session_id"].as_str().unwrap()).unwrap();
    let (code, _, messages) = invoke(
        &app,
        "GET",
        "/slack/conversations/TBETA.CCHAT/messages",
        auth,
        Some(beta),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(messages["items"][0]["text"], "Bearer xoxp-TBETA");
    assert_eq!(messages["items"][0]["author"]["id"], "TBETA.U2");
    let (code, _, error) = invoke(
        &app,
        "GET",
        "/slack/conversations/TBETA.DSHARED/messages",
        auth,
        Some(alpha),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "slack-session-changed");

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
    assert_eq!(url.query(), Some("team=TALPHA"));
    assert_eq!(
        invoke(
            &app,
            "POST",
            "/slack/token-import",
            url.fragment(),
            None,
            None,
            Some(json!({"team_id":"TALPHA","token":"xoxp-TWRONG"}))
        )
        .await
        .0,
        StatusCode::CONFLICT
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
    let client = app.clone();
    let secret = auth.unwrap().to_owned();
    let update = tokio::spawn(async move {
        invoke(&client, "POST", action, Some(&secret), None, None, None).await
    });
    let url = Url::parse(
        &timeout(Duration::from_secs(2), opened.recv())
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        invoke(
            &app,
            "POST",
            "/slack/token-import",
            url.fragment(),
            None,
            None,
            Some(json!({"team_id":"TALPHA","token":"xoxp-TALPHA-renewed"}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(update.await.unwrap().0, StatusCode::OK);
    let saved = base
        .read_record_password("slack", "teams", "TALPHA", "token")
        .await
        .unwrap();
    assert_eq!(saved.trim_end(), "xoxp-TALPHA-renewed");
    let service = projects.service("TALPHA").await.unwrap();
    let session = service.session_id().await.unwrap();
    assert_eq!(
        service.access(session).await.unwrap(),
        "xoxp-TALPHA-renewed"
    );
    assert_eq!(session, alpha);
    assert!(stored_keychains.lock().unwrap().values().all(|chain| {
        chain
            .0
            .lock()
            .unwrap()
            .account
            .as_ref()
            .is_none_or(|account| account.access.is_empty())
    }));
    for _ in 0..2 {
        let (status, _, _) = invoke(
            &app,
            "DELETE",
            "/data/slack/teams/TALPHA",
            auth,
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None, None).await;
    assert_eq!(config["value"].as_array().unwrap().len(), 1);
    assert_eq!(config["value"][0]["team_id"], "TBETA");
    assert!(projects.service("TALPHA").await.is_err());
    assert!(projects.service("TBETA").await.is_ok());

    projects.stop().await;
    worker.await.unwrap();
    upstream.abort();
}

#[tokio::test]
async fn composition_mounts_flat_slack_routes() {
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
        api["method"] == "POST" && api["path"] == "/slack/open" && api["access"] == "local"
    }));
    let (status, _, body) = invoke(&app, "GET", "/slack/open", None, None, None, None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["error"]["code"], "method-not-allowed");
    assert!(meta["apis"].as_array().unwrap().iter().all(|api| {
        let path = api["path"].as_str().unwrap();
        path != "/slack/status" && !path.starts_with("/slack/teams/")
    }));
}
