use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
};
use futures_util::SinkExt;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    time::{sleep, timeout},
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message,
        handshake::server::{Request as WebSocketRequest, Response as WebSocketResponse},
    },
};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    modules::{
        config::service::ConfigService, notification::NotificationService, status::StatusService,
    },
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

#[derive(Debug)]
#[derive(Default)]
struct OpenSlackRunner {
    deliveries: Mutex<Vec<Value>>,
    emacs_exited: AtomicBool,
}

impl crate::modules::emacs::service::CommandRunner for OpenSlackRunner {
    fn run<'a>(
        &'a self,
        _: &'a str,
        args: &'a [String],
    ) -> crate::modules::emacs::service::ServiceFuture<
        'a,
        crate::modules::emacs::service::CommandOutput,
    > {
        Box::pin(async move {
            assert_eq!(args[0], "--alternate-editor=/usr/bin/false");
            let decode_string = |source: &str| {
                serde_json::Deserializer::from_str(source)
                    .into_iter::<String>()
                    .next()
                    .unwrap()
                    .unwrap()
            };
            let script = decode_string(args[3].split_once("((source ").unwrap().1);
            if let Some((_, argument)) = script.split_once("(loki-assistant-slack-dispatch-messages ") {
                use std::os::unix::fs::PermissionsExt;
                let file = decode_string(argument);
                assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
                let payload = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
                self.deliveries.lock().unwrap().push(payload);
                let exited = self.emacs_exited.load(Ordering::SeqCst);
                return Ok(crate::modules::emacs::service::CommandOutput {
                    success: !exited,
                    status: Some(if exited { 1 } else { 0 }),
                    stdout: if exited { String::new() }
                        else { json!(json!({"result":"applied"}).to_string()).to_string() },
                    stderr: String::new(),
                });
            }
            assert!(script.contains("TTEAM.DDIRECT"));
            let target = decode_string(
                script
                    .split_once("(loki-assistant-slack-open-notification ")
                    .unwrap()
                    .1,
            );
            let target: Value = serde_json::from_str(&target).unwrap();
            let result = if target["message_ts"].is_string() {
                assert_eq!(target["reply"]["text"], "线程回复");
                "message"
            } else if target["thread_ts"].is_string() {
                assert_eq!(target["root"]["text"], "问候");
                "thread"
            } else {
                "conversation"
            };
            Ok(crate::modules::emacs::service::CommandOutput {
                success: true,
                status: Some(0),
                stdout: json!(json!({"result":result}).to_string()).to_string(),
                stderr: String::new(),
            })
        })
    }
}

use super::{
    projects::{Backend, Projects},
    service::now,
};

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
    posts: AtomicUsize,
    history_unavailable: AtomicBool,
}

#[test]
fn disk_cache_retains_active_threads_and_restores_message_records() {
    use std::{io::Write, time::SystemTime};

    use super::{disk::DiskCache, protocol::Message};

    let directory = Directory::new();
    let cache = DiskCache::open(&directory.0, "TTEAM", "UME").unwrap();
    let mut room = super::store::Conversation::new("CROOM", "public_channel", "频道");
    super::disk::atomic_write(
        &cache.room_path("CROOM"),
        b"((id . \"CROOM\") (kind . \"public_channel\") (title . \"Room\") (participants . []))\n",
    )
    .unwrap();
    assert!(cache.rooms()["CROOM"].read_positions.baseline.is_none());
    room.read_positions.baseline = Some(10);
    room.read_positions
        .cursors
        .insert(String::new(), "10.000001".into());
    room.read_positions
        .cursors
        .insert("10.000001".into(), "11.000002".into());
    cache.save_room(&room).unwrap();
    let root = Message::from_slack(
        &json!({
            "ts":"10.000001", "text":"UTF-8 文本\n引号\"与反斜杠\\",
            "files":[{"id":"F1","name":"报告.pdf","mimetype":"application/pdf",
                      "url_private_download":"https://files.slack.com/download"}]
        }),
        None,
    )
    .unwrap();
    let reply = Message::from_slack(
        &json!({"ts":"11.000002","thread_ts":root.ts,"text":"回复"}),
        None,
    )
    .unwrap();
    let stale = Message::from_slack(&json!({"ts":"1.000003","text":"旧消息"}), None).unwrap();
    for message in [&root, &reply, &stale] {
        cache.save_message("CROOM", message);
    }
    let attachment = &root.attachments[0];
    let (pending, mut file) = cache.begin_original(attachment, "application/pdf").unwrap();
    assert!(cache.attachment(attachment, false).is_none());
    file.write_all(b"original").unwrap();
    drop(file);
    let path = pending.commit().unwrap();
    assert!(path.ends_with("F1.original.pdf"));
    assert_eq!(cache.attachment(attachment, false), Some(path.clone()));
    let loaded = cache.rooms().remove("CROOM").unwrap();
    let saved = &loaded.messages[&root.ts];
    assert_eq!(saved.text, root.text);
    assert_eq!(saved.attachments[0].download_url, attachment.download_url);

    let room_path = directory.0.join("TTEAM/conversations/CROOM");
    let root_path = room_path.join("messages/10.000001.txt");
    let reply_path = room_path.join("threads/10.000001/11.000002.txt");
    let stale_path = room_path.join("messages/1.000003.txt");
    let expire = |path: &std::path::Path| {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(2 * 86_400))
            .unwrap();
    };
    expire(&root_path);
    expire(&stale_path);
    expire(&path);
    DiskCache::clean(&directory.0, Duration::from_secs(86_400));
    assert!(root_path.is_file() && reply_path.is_file());
    assert!(!stale_path.exists() && !path.exists());

    expire(&reply_path);
    DiskCache::clean(&directory.0, Duration::from_secs(86_400));
    assert!(!root_path.exists() && !reply_path.exists());
    assert!(cache.room_path("CROOM").is_file());
    let rooms = cache.rooms();
    let restored = &rooms["CROOM"].read_positions;
    assert_eq!(restored.baseline, Some(10));
    assert_eq!(restored.get(None), Some("10.000001"));
    assert_eq!(restored.get(Some("10.000001")), Some("11.000002"));
    let other = DiskCache::open(&directory.0, "TTEAM", "UOTHER").unwrap();
    assert!(other.rooms().is_empty());
}

async fn invoke(
    app: &Router,
    method: &str,
    path: &str,
    session: Option<Uuid>,
    idempotency_key: Option<Uuid>,
    payload: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:47321");
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
                && service.session_id().await.is_some()
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
async fn socket_notifications_threads_and_idempotent_sends() {
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
        posts: AtomicUsize::default(),
        history_unavailable: AtomicBool::default(),
    });
    let slack = Router::new()
        .route("/rtm.connect", post(|State(mock): State<Arc<Mock>>, headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer xoxc-test-user");
            assert_eq!(headers["cookie"], "d=first");
            Json(json!({"ok":true,"url":mock.socket}))
        }))
        .route("/auth.test", post(|headers: HeaderMap| async move {
            assert_eq!(headers["authorization"], "Bearer xoxc-test-user");
            assert_eq!(headers["cookie"], "d=first");
            Json(json!({"ok":true,"team_id":"TTEAM","user_id":"UME","team":"工作区"}))
        }))
        .route("/users.conversations", get(|| async {
            Json(json!({"ok":true,"channels":[
                {"id":"DDIRECT","user":"UYOU","is_im":true},
                {"id":"CWORK","name":"产品讨论","is_channel":true}
            ]}))
        }))
        .route("/users.info", get(|| async { Json(json!({"ok":true,"user":{"real_name":"林舟"}})) }))
        .route("/conversations.history", get(|State(mock): State<Arc<Mock>>, Query(query): Query<std::collections::HashMap<String, String>>| async move {
            if mock.history_unavailable.load(Ordering::SeqCst) {
                return Json(json!({"ok":false,"error":"internal_error"}));
            }
            if query.get("channel").is_some_and(|channel| channel == "CWORK") {
                return Json(json!({"ok":true,"messages":[]}));
            }
            let root = json!({"ts":mock.root,"text":"问候","user":"UYOU","reply_count":2});
            let response = if query.contains_key("inclusive") {
                if query.get("latest") == Some(&mock.root) { json!({"ok":true,"messages":[root]}) }
                else { json!({"ok":true,"messages":[]}) }
            } else if query.get("latest") == Some(&mock.root) {
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
            let response = if query.get("oldest") == Some(&earlier) || query.get("oldest") == Some(&mock.reply) {
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
            assert_eq!(headers["authorization"], "Bearer xoxc-test-user");
            assert_eq!(headers["cookie"], "d=first");
            assert_eq!(body["thread_ts"], mock.root);
            assert_eq!(body["text"], "收到");
            mock.posts.fetch_add(1, Ordering::SeqCst);
            Json(json!({"ok":true,"ts":mock.sent,"message":{"ts":mock.sent,"thread_ts":mock.root,
                        "text":"收到","user":"UME"}}))
        })).with_state(Arc::clone(&mock));
    let http = tokio::spawn(async {
        axum::serve(web, slack).await.unwrap();
    });
    let (done, messages_sent) = tokio::sync::oneshot::channel();
    let (send_channel, channel_ready) = tokio::sync::oneshot::channel();
    let socket_mock = Arc::clone(&mock);
    let socket = tokio::spawn(async move {
        let mut stream = accept_hdr_async(
            sockets.accept().await.unwrap().0,
            |request: &WebSocketRequest, response: WebSocketResponse| {
                assert_eq!(request.headers()["cookie"], "d=first");
                Ok(response)
            },
        )
        .await
        .unwrap();
        stream
            .send(Message::Text(r#"{"type":"hello"}"#.into()))
            .await
            .unwrap();
        let message = Message::Text(
            json!({"type":"message","channel":"DDIRECT","ts":socket_mock.root,
                   "user":"UYOU","text":"问候"})
            .to_string()
            .into(),
        );
        stream.send(message.clone()).await.unwrap();
        stream.send(message).await.unwrap();
        let _ = done.send(());
        channel_ready.await.unwrap();
        let ordinary = json!({"type":"message","channel":"CWORK","ts":socket_mock.root,
            "user":"UYOU","text":"<!channel> 请 <@UMEOTHER> 看一下"});
        let mention = json!({"type":"message","channel":"CWORK","ts":socket_mock.reply,
            "thread_ts":socket_mock.root,"user":"UYOU","text":"请 <@UME> 看一下"});
        for payload in [&ordinary, &mention, &mention] {
            stream
                .send(Message::Text(payload.to_string().into()))
                .await
                .unwrap();
        }
        std::future::pending::<()>().await;
    });

    let alerts = Arc::new(Alerts::default());
    let notification = Arc::new(NotificationService::with_launcher(
        alerts.clone(),
        Duration::from_secs(1),
    ));
    let (config, passwords) = password_config(&directory.0.join("config.yml"), &directory.0).await;
    let credentials = "loki-assistant/slack/TTEAM.gpg";
    passwords
        .save_multiline(credentials, "xoxc-test-user\nd_cookie: first")
        .await
        .unwrap();
    config
        .replace_record(
            "slack".into(),
            "teams".into(),
            "TTEAM".into(),
            json!({"name":"工作区","token":credentials,"d_cookie":credentials}),
            true,
            true,
        )
        .await
        .unwrap();
    let status = Arc::new(StatusService::new());
    config
        .replace("emacs", "emacsclient_path", json!("/usr/bin/true"))
        .await
        .unwrap();
    let emacs = Arc::new(OpenSlackRunner::default());
    let start = || {
        let projects = Arc::new(Projects::with_backend(
            Arc::clone(&config),
            Arc::clone(&notification),
            Arc::clone(&status),
            Arc::new(crate::modules::emacs::service::EmacsService::with_runner(
                crate::modules::emacs::config::EmacsConfig::new(config.clone()),
                notification.clone(),
                status.clone(),
                emacs.clone(),
                Duration::from_secs(1),
            )),
            Backend {
                api: api.clone(),
                cache_root: Some(directory.0.join("cache")),
                open_browser: Arc::new(|_| unreachable!()),
            },
        ));
        let app = projects.router().merge(status.router());
        let worker = tokio::spawn(Arc::clone(&projects).run(47321));
        (projects, app, worker)
    };
    let (projects, app, worker) = start();
    let service = ready_service(&projects, "TTEAM").await;
    let (status_code, headers, _) =
        invoke(&app, "GET", "/slack/conversations", None, None, None).await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    timeout(Duration::from_secs(5), messages_sent)
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
    assert_eq!(alerts.0.lock().unwrap()[0].app.as_deref(), Some("slack"));
    let navigation = alerts.0.lock().unwrap()[0].navigation_url.clone().unwrap();
    let navigation = url::Url::parse(&navigation).unwrap();
    let (_, callback) = navigation
        .query_pairs()
        .find(|(key, _)| key == "callback")
        .unwrap();
    assert_eq!(
        callback,
        format!(
            "/slack/conversations/TTEAM.DDIRECT/open?message_ts={}",
            mock.root
        )
    );
    for (query, expected) in [
        (
            format!("thread_ts={}&message_ts={}", mock.root, mock.reply),
            "message",
        ),
        (format!("thread_ts={}&message_ts=1.0", mock.root), "thread"),
        ("thread_ts=1.0".into(), "conversation"),
    ] {
        let (status, _, body) = invoke(
            &app,
            "POST",
            &format!("/slack/conversations/TTEAM.DDIRECT/open?{query}"),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["target"], expected);
    }

    let data = invoke(&app, "GET", "/status/slack/connections", None, None, None)
        .await
        .2;
    assert_eq!(data["value"][0]["authorization_state"], "authorized");
    assert_eq!(data["value"][0]["socket_state"], "connected");
    let session = Some(Uuid::parse_str(data["value"][0]["session_id"].as_str().unwrap()).unwrap());

    let timeline = "/slack/conversations/TTEAM.DDIRECT/messages";
    let thread = format!(
        "/slack/conversations/TTEAM.DDIRECT/threads/{}/messages",
        mock.root
    );
    let read = format!(
        "/slack/conversations/TTEAM.DDIRECT/threads/{}/read-position",
        mock.root
    );
    let (_, _, view) = invoke(&app, "GET", timeline, None, None, None).await;
    assert_eq!(view["items"][0]["ts"], mock.root);
    let (status, _, view) = invoke(
        &app,
        "GET",
        &format!("{timeline}?limit=1"),
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
        session,
        None,
        None,
    )
    .await;
    assert_eq!(view["root"]["ts"], mock.root);
    assert_eq!(view["root"]["author"]["id"], "TTEAM.UYOU");
    assert_eq!(view["items"][0]["ts"], mock.reply);
    let reply_path = directory.0.join(format!(
        "cache/TTEAM/conversations/DDIRECT/threads/{}/{}.txt",
        mock.root, mock.reply
    ));
    assert_eq!(view["items"][0]["cache_path"], json!(reply_path));
    assert!(fs::read_to_string(reply_path).unwrap().contains("线程回复"));
    let thread_cursor = view["next_cursor"].as_str().unwrap();
    let (_, _, view) = invoke(
        &app,
        "GET",
        &format!("{thread}?limit=1&cursor={thread_cursor}"),
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
        None,
        None,
        None,
    )
    .await;
    assert_eq!(view["items"][0]["unread_count"], 1);

    let subscription = Uuid::new_v4();
    let subscribe = format!("/slack/conversations/TTEAM.CWORK/subscriptions/{subscription}");
    assert_eq!(invoke(&app, "PUT", &subscribe, None, None, None).await.0, StatusCode::BAD_REQUEST);
    let (code, _, created) = invoke(&app, "PUT", &subscribe, session, None, None).await;
    assert_eq!(code, StatusCode::CREATED);
    assert_eq!(created["subscription_id"], json!(subscription));
    assert_eq!(invoke(&app, "PUT", &subscribe, session, None, None).await.0, StatusCode::OK);
    timeout(Duration::from_secs(3), async {
        while emacs.deliveries.lock().unwrap().is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(emacs.deliveries.lock().unwrap()[0]["kind"], "snapshot");
    assert_eq!(emacs.deliveries.lock().unwrap()[0]["sequence"], 1);

    send_channel.send(()).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let delivered = emacs.deliveries.lock().unwrap().iter().any(|batch| {
                batch["kind"] == "changes" && batch["items"].as_array().unwrap().iter().any(|item| {
                    item["ts"] == mock.root && item["author"]["id"] == "TTEAM.UYOU"
                        && item["reply_count"] == 1
                })
            });
            if delivered { break; }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    emacs.emacs_exited.store(true, Ordering::SeqCst);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    timeout(Duration::from_secs(3), async {
        while service.state.lock().await.cache.subscriptions.contains_key(&("CWORK".into(), subscription)) {
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    tokio::time::resume();
    assert_eq!(emacs.deliveries.lock().unwrap().last().unwrap()["kind"], "probe");
    assert_eq!(invoke(&app, "DELETE", &subscribe, None, None, None).await.0, StatusCode::NO_CONTENT);

    let channel = timeout(Duration::from_secs(3), async {
        loop {
            let (_, _, data) = invoke(
                &app,
                "GET",
                "/slack/conversations?kind=public_channel",
                None,
                None,
                None,
            )
            .await;
            let row = &data["items"][0];
            if row["last_unread_mention_ts"] == mock.reply {
                break row.clone();
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("实时频道提及进入会话摘要");
    assert_eq!(channel["unread_count"], 2);
    assert_eq!(channel["last_message"]["author"]["id"], "TTEAM.UYOU");
    assert_eq!(channel["last_message"]["preview"], "请 <@UME> 看一下");
    let navigation = timeout(Duration::from_secs(3), async {
        loop {
            if let Some(alert) = alerts.0.lock().unwrap().get(1) {
                break alert.navigation_url.clone().unwrap();
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("频道提及触发通知");
    let navigation = url::Url::parse(&navigation).unwrap();
    let (_, callback) = navigation
        .query_pairs()
        .find(|(key, _)| key == "callback")
        .unwrap();
    assert_eq!(
        callback,
        format!(
            "/slack/conversations/TTEAM.CWORK/open?message_ts={}&thread_ts={}",
            mock.reply, mock.root
        )
    );

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
        invoke(&app, "POST", &thread, None, key, Some(body.clone())),
        invoke(&app, "POST", &thread, None, key, Some(body.clone()))
    );
    assert_eq!(first.0, StatusCode::ACCEPTED);
    assert!(matches!(second.0, StatusCode::ACCEPTED | StatusCode::OK));
    assert_eq!(first.1["location"], second.1["location"]);
    let send = first.1["location"].to_str().unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if invoke(&app, "GET", send, None, None, None).await.2["state"] == "sent" {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(mock.posts.load(Ordering::SeqCst), 1);
    assert_eq!(alerts.0.lock().unwrap().len(), 2);
    assert_eq!(
        invoke(
            &app,
            "POST",
            &thread,
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

    mock.history_unavailable.store(true, Ordering::SeqCst);
    let (projects, app, worker) = start();
    let service = ready_service(&projects, "TTEAM").await;
    let fresh_session = service.session_id().await;
    assert_ne!(fresh_session, session);
    let recovered = Uuid::new_v4();
    let recovered_path = format!("/slack/conversations/TTEAM.DDIRECT/subscriptions/{recovered}");
    assert_eq!(
        invoke(&app, "PUT", &recovered_path, session, None, None).await.0,
        StatusCode::CONFLICT
    );
    emacs.emacs_exited.store(false, Ordering::SeqCst);
    assert_eq!(
        invoke(&app, "PUT", &recovered_path, fresh_session, None, None).await.0,
        StatusCode::CREATED
    );
    timeout(Duration::from_secs(3), async {
        loop {
            if emacs.deliveries.lock().unwrap().iter().any(|batch| {
                batch["subscription_id"] == json!(recovered)
                    && batch["sequence"] == 1
                    && batch["snapshot"]["sync_state"] == "cached"
            }) {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert_eq!(
        invoke(&app, "DELETE", &recovered_path, None, None, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(service.state.lock().await.cache.subscriptions.is_empty());
    let (code, _, restored) = invoke(&app, "GET", timeline, fresh_session, None, None).await;
    assert_eq!(code, StatusCode::OK, "{restored}");
    assert_eq!(restored["sync_state"], "cached");
    assert!(restored["read_through_ts"].is_null());
    assert_eq!(restored["first_unread_ts"], mock.root);
    assert!(restored["items"].as_array().unwrap().iter().any(|item| {
        item["ts"] == mock.root
            && item["cache_path"]
                .as_str()
                .is_some_and(|path| std::path::Path::new(path).is_file())
    }));
    let (code, _, restored_thread) = invoke(&app, "GET", &thread, fresh_session, None, None).await;
    assert_eq!(code, StatusCode::OK, "{restored_thread}");
    assert_eq!(restored_thread["read_through_ts"], mock.reply);
    assert!(restored_thread["first_unread_ts"].is_null());
    assert_eq!(
        invoke(&app, "GET", send, session, None, None).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        invoke(&app, "GET", send, fresh_session, None, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(!fs::read_dir(&directory.0).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("slack-")
    }));
    assert_eq!(
        invoke(
            &app,
            "DELETE",
            "/slack/session?team_id=TTEAM",
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        invoke(&app, "GET", timeline, session, None, None).await.0,
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
            if args[0] == "rm" {
                assert_eq!(&args[..3], ["rm", "--force", "--"]);
                fs::remove_file(store.join(format!("{}.gpg", args[3]))).unwrap();
                return Ok(crate::modules::password::service::CommandOutput {
                    status: Some(0),
                    stdout: Vec::new(),
                });
            }
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

async fn password_config(
    config_path: &std::path::Path,
    store: &std::path::Path,
) -> (
    Arc<ConfigService>,
    Arc<crate::modules::password::service::PasswordService>,
) {
    use crate::modules::password::{config::PasswordConfig, service::PasswordService};

    let config = Arc::new(ConfigService::load(config_path).unwrap());
    config
        .replace("password", "store_dir", json!(store.to_str().unwrap()))
        .await
        .unwrap();
    config
        .replace("password", "pass_path", json!("/bin/echo"))
        .await
        .unwrap();
    let passwords = Arc::new(
        PasswordService::with_runner(
            PasswordConfig::new(config.clone()),
            Arc::new(FakePass),
            Duration::from_secs(2),
        )
        .with_writer(fake_pass_write),
    );
    let config = Arc::new((*config).clone().with_password_service(passwords.clone()));
    (config, passwords)
}

#[tokio::test]
async fn pending_team_passwords_migrate_to_team_id_filename() {
    let directory = Directory::new();
    let config_path = directory.0.join("config.yml");
    let (_, passwords) = password_config(&config_path, &directory.0).await;
    let id = uuid::Uuid::new_v4();
    let path = "loki-assistant/slack/TLEGACY.gpg";
    let legacy = format!("loki-assistant/slack/TLEGACY-{id}.gpg");
    passwords
        .save_multiline(&legacy, "xoxc-fixture")
        .await
        .unwrap();
    let mut values: Value =
        serde_yaml_ng::from_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    values["_pending_config_notifications"] = json!({
        (id.to_string()): {
            "id":id,"module":"slack","item":"teams","instance":"TLEGACY","key":"TLEGACY",
            "value":{"team_id":"TLEGACY","token":legacy},"previous":null,"delete":false
        }
    });
    fs::write(&config_path, serde_yaml_ng::to_string(&values).unwrap()).unwrap();

    let (config, _) = password_config(&config_path, &directory.0).await;
    config.migrate_pending_password_paths().await.unwrap();
    config.migrate_pending_password_paths().await.unwrap();

    let app =
        crate::modules::config::Module::from_service(ConfigService::load(&config_path).unwrap())
            .router();
    let (_, _, response) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert_eq!(response["pending"][0]["status"], "pending");
    assert_eq!(response["pending"][0]["value"]["token"], path);
    assert!(response["pending"][0]["value"].get("d_cookie").is_none());
    assert_eq!(passwords.fetch(path).await.unwrap(), "xoxc-fixture\n");
    assert!(!directory.0.join(legacy).exists());
}

#[tokio::test]
async fn project_workflow_uses_metadata_isolated_passwords_and_team_validation() {
    use crate::modules::config;

    let directory = Directory::new();
    let cache_root = directory.0.join("retention-test");
    let cached = super::disk::DiskCache::open(&cache_root, "TOLD", "UME").unwrap();
    let old = super::protocol::Message::from_slack(&json!({"ts":"1.000001","text":"旧消息"}), None)
        .unwrap();
    cached.save_message("CROOM", &old);
    let old_path = cache_root.join("TOLD/conversations/CROOM/messages/1.000001.txt");
    fs::File::options()
        .write(true)
        .open(&old_path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(2 * 86_400))
        .unwrap();
    let config_path = directory.0.join("config.yml");
    let (base, passwords) = password_config(&config_path, &directory.0).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let user_directory_requests = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let requested_teams = Arc::clone(&user_directory_requests);
    let user_info_requests = Arc::new(Mutex::new(Vec::new()));
    let requested_users = Arc::clone(&user_info_requests);
    let history_requests = Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    let requested_rooms = Arc::clone(&history_requests);
    let slack = Router::new()
        .route(
            "/users.info",
            get(move |headers: HeaderMap, Query(query): Query<std::collections::BTreeMap<String, String>>| {
                let requested_users = Arc::clone(&requested_users);
                async move {
                    let user = &query["user"];
                    requested_users.lock().unwrap().push((headers["authorization"].to_str().unwrap().to_owned(), user.clone()));
                    Json(match user.as_str() {
                        "UADDED" => json!({"ok":true,"user":{"id":user,"real_name":"补查用户"}}),
                        "UMISSING" => json!({"ok":false,"error":"user_not_found"}),
                        _ => json!({"ok":false,"error":"internal_error"}),
                    })
                }
            }),
        )
        .route(
            "/users.list",
            get(move |headers: HeaderMap| {
                let requested_teams = Arc::clone(&requested_teams);
                async move {
                    requested_teams.lock().unwrap().insert(headers["authorization"].to_str().unwrap().to_owned());
                    Json(json!({"ok":true,"members":[
                        {"id":"U1","name":"linzhou","profile":{"display_name":"林舟","image_48":"https://avatars.slack-edge.com/avatar.png"}},
                        {"id":"U2","name":"linzhou","profile":{"display_name":"林舟","image_48":"https://avatars.slack-edge.com/avatar.png"}},
                        {"id":"UOLD","deleted":true}
                    ]}))
                }
            }),
        )
        .route(
            "/users.conversations",
            get(|| async {
                Json(json!({"ok":true,"channels":[
                    {"id":"DSHARED","user":"U2","is_im":true},
                    {"id":"CCHAT","name":"项目讨论","is_channel":true},
                    {"id":"GCHAT","name":"内部讨论","is_channel":true,"is_private":true}
                ]}))
            }),
        )
        .route(
            "/conversations.history",
            get(move |headers: HeaderMap, Query(query): Query<std::collections::BTreeMap<String, String>>| {
                let requested_rooms = Arc::clone(&requested_rooms);
                async move {
                    let identity = headers["authorization"].to_str().unwrap();
                    requested_rooms.lock().unwrap().insert((identity.to_owned(), query["channel"].clone()));
                    Json(json!({"ok":true,"messages":[{"ts":"100.000001","text":identity,"user":"U2"}]}))
                }
            }),
        )
        .route(
            "/rtm.connect",
            post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/auth.test",
            post(|headers: HeaderMap| async move {
                let authorization = headers["authorization"].to_str().unwrap();
                let team = if let Some(web) = authorization.strip_prefix("Bearer xoxc-") {
                    assert!(matches!(
                        headers["cookie"].to_str().unwrap(),
                        "d=test-web-cookie" | "d=refreshed-web-cookie"
                    ));
                    web
                } else {
                    authorization.strip_prefix("Bearer xoxp-").unwrap()
                }
                .split('-')
                .next()
                .unwrap();
                Json(json!({"ok":true,"team_id":team,"team":"网页协作","user_id":"U1"}))
            }),
        );
    let upstream = tokio::spawn(async move {
        axum::serve(listener, slack).await.unwrap();
    });
    let status = Arc::new(StatusService::new());
    let (browser, mut opened) = tokio::sync::mpsc::unbounded_channel::<String>();
    let projects = Arc::new(Projects::with_backend(
        base.clone(),
        Arc::new(NotificationService::new()),
        status.clone(),
        crate::modules::emacs::Module::new(
            base.clone(),
            Arc::new(NotificationService::new()),
            status.clone(),
        )
        .service(),
        Backend {
            api,
            cache_root: Some(cache_root),
            open_browser: Arc::new(move |url| {
                let browser = browser.clone();
                Box::pin(async move {
                    browser.send(url).unwrap();
                    Ok(())
                })
            }),
        },
    ));
    let config_module = config::Module::from_service(
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
        .merge(config_module.router())
        .merge(projects.router());
    let notification_router = Arc::new(super::routes::config_notification_router(projects.clone()));
    config_module
        .service()
        .attach_notifications(&notification_router);
    let worker = tokio::spawn(Arc::clone(&projects).run(47321));
    let meta = invoke(&app, "GET", "/meta", None, None, None).await.2;
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
    assert_eq!(teams["structure"]["show_all_fields"], true);
    let fields = teams["structure"]["fields"].as_array().unwrap();
    for (name, data_type) in [
        (
            "token",
            json!({"password":{"path":"loki-assistant/slack/{key}.gpg"}}),
        ),
        (
            "d_cookie",
            json!({"password":{"path":"loki-assistant/slack/{key}.gpg","key":"d_cookie"}}),
        ),
    ] {
        let field = fields.iter().find(|field| field["key"] == name).unwrap();
        assert_eq!(field["data_type"], data_type);
        assert!(field.get("password_path").is_none() && field.get("password_line").is_none());
    }
    assert_eq!(
        teams["structure"]["fields"][0]["checker"]["pattern"],
        "^T[A-Z0-9]{1,47}$"
    );

    let token_field = fields.iter().find(|field| field["key"] == "token").unwrap();
    assert_eq!(
        token_field["checker"]["pattern"],
        super::client::WEB_TOKEN_PATTERN
    );

    assert!(teams.get("actions").is_none());
    let items = module["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let retention = items
        .iter()
        .find(|item| item["key"] == "cache_retention_days")
        .unwrap();
    assert_eq!(retention["data_type"], "integer");
    assert_eq!(retention["default"], 30);
    let path = "/data/slack/cache_retention_days";
    let (_, _, default) = invoke(&app, "GET", path, None, None, None).await;
    assert_eq!(default["value"], 30);
    let (_, _, invalid) = invoke(&app, "PUT", path, None, None, Some(json!(0))).await;
    assert_eq!(invalid["error"]["code"], "config-change-rejected");
    assert!(old_path.is_file());
    let (code, _, saved) = invoke(&app, "PUT", path, None, None, Some(json!(1))).await;
    assert_eq!(code, StatusCode::OK, "{saved}");
    assert_eq!(saved["value"], 1);
    timeout(Duration::from_secs(3), async {
        while old_path.is_file() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        invoke(
            &app,
            "POST",
            "/config/actions/slack/authorize/TPROBE",
            None,
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let create = |team: &str, body: Value| {
        Request::builder()
            .method("PUT")
            .uri(format!("/data/slack/teams/{team}"))
            .header("if-none-match", "*")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    assert_eq!(teams["structure"]["id_field"], "team_id");
    assert_eq!(teams["structure"]["fields"][0]["required"], true);
    for request in [
        create("TALPHA", json!({"name":"首次新增只能提交 Team ID"})),
        create("INVALID", json!({})),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert!(opened.try_recv().is_err());
    for (team, first_field, second_field) in [
        ("TALPHA", "d_cookie", "token"),
        ("TBETA", "token", "d_cookie"),
    ] {
        let team_path = format!("/data/slack/teams/{team}");
        let setup_url = format!("http://127.0.0.1:47321/slack/team-setup/{team}");
        let token = format!("xoxc-{team}");
        let cookie = "test-web-cookie";
        let created = app.clone().oneshot(create(team, json!({}))).await.unwrap();
        assert_eq!(created.status(), StatusCode::ACCEPTED);
        assert_eq!(opened.try_recv().unwrap(), setup_url);
        let credentials = json!({"token":token,"d_cookie":cookie});
        let (status, _, partial) = invoke(
            &app,
            "PATCH",
            &team_path,
            None,
            None,
            Some(json!({(first_field): credentials[first_field]})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(partial["status"], "pending");
        assert_eq!(opened.try_recv().unwrap(), setup_url);
        assert!(partial["value"].get(second_field).is_none());
        let staged_path = partial["value"][first_field].as_str().unwrap();
        let confirmed_path = format!("loki-assistant/slack/{team}.gpg");
        assert_eq!(staged_path, confirmed_path);
        assert_eq!(
            passwords.fetch(staged_path).await.unwrap(),
            if first_field == "d_cookie" {
                format!("\nd_cookie: {cookie}\n")
            } else {
                format!("{token}\n")
            }
        );
        let (status, _, outcome) = invoke(
            &app,
            "PATCH",
            &team_path,
            None,
            None,
            Some(json!({(second_field): credentials[second_field]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(outcome["status"], "confirmed");
        let path = outcome["value"]["token"].as_str().unwrap();
        assert_eq!(path, confirmed_path);
        assert_eq!(outcome["value"]["d_cookie"], path);
        assert_eq!(
            passwords.fetch(path).await.unwrap(),
            format!("{token}\nd_cookie: test-web-cookie\n")
        );
        assert!(!outcome.to_string().contains("xoxc-"));
        assert!(!fs::read_to_string(&config_path).unwrap().contains("xoxc-"));
        let duplicate = app.clone().oneshot(create(team, json!({}))).await.unwrap();
        assert_eq!(duplicate.status(), StatusCode::PRECONDITION_FAILED);
    }
    timeout(Duration::from_secs(4), async {
        loop {
            let connections = status.value("slack", "connections").unwrap().value;
            if connections
                .as_array()
                .unwrap()
                .iter()
                .filter(|project| project["authorization_state"] == "authorized")
                .count()
                == 2
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let invalid = invoke(
        &app,
        "POST",
        "/data/slack/teams",
        None,
        None,
        Some(json!([{"team_id":"wrong","name":"check"}])),
    )
    .await;
    assert_eq!(invalid.0, StatusCode::BAD_REQUEST);
    assert_eq!(invalid.2["error"]["code"], "invalid-config");
    let public = status.value("slack", "connections").unwrap().value;
    assert_eq!(public[0]["team_id"], "TALPHA");
    assert_eq!(public[1]["team_id"], "TBETA");
    assert_eq!(public[0]["user_id"], "TALPHA.U1");
    assert_eq!(public[1]["user_id"], "TBETA.U1");

    timeout(Duration::from_secs(3), async {
        while user_directory_requests.lock().unwrap().len() < 2
            || history_requests.lock().unwrap().len() < 6
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("WebSocket 不可用时，两个工作区仍在首次查询前预热用户目录与会话消息");
    let picker = timeout(Duration::from_secs(3), async {
        loop {
            let (status, headers, body) =
                invoke(&app, "GET", "/slack/conversations/picker", None, None, None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(headers["cache-control"], "no-store");
            if body["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .all(|team| team["state"] == "ready")
                && body["items"].as_array().unwrap().len() == 6
            {
                break body;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let im = picker["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "im")
        .unwrap();
    assert_eq!(im["participants"][0]["name"], "林舟");
    assert_eq!(
        im["participants"][0]["avatar_url"],
        "https://avatars.slack-edge.com/avatar.png"
    );
    assert!(
        im["search_terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|term| term == "lz")
    );
    let (_, _, users) = invoke(&app, "GET", "/slack/users?limit=1", None, None, None).await;
    assert_eq!(users["items"][0]["id"], "TALPHA.U1");
    assert_eq!(users["items"][0]["name"], "林舟");
    assert_eq!(users["items"][0]["username"], "linzhou");
    assert_eq!(
        users["items"][0]["avatar_url"],
        "https://avatars.slack-edge.com/avatar.png"
    );
    let cursor = users["next_cursor"].as_str().unwrap();
    let (_, _, users) = invoke(
        &app,
        "GET",
        &format!("/slack/users?cursor={cursor}"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(users["items"][0]["id"], "TALPHA.U2");
    assert_eq!(users["items"][1]["id"], "TBETA.U1");
    assert_eq!(users["items"][2]["id"], "TBETA.U2");
    assert_eq!(users["items"].as_array().unwrap().len(), 3);
    assert!(users["next_cursor"].is_null());

    for query in [
        "id=".to_owned(),
        "id=U1".to_owned(),
        "id=TALPHA.U1&id=wrong".to_owned(),
        "id=TALPHA.U1&limit=1".to_owned(),
        "id=TALPHA.U1&cursor=page".to_owned(),
        vec!["id=TALPHA.U1"; 101].join("&"),
    ] {
        let (code, _, body) = invoke(
            &app,
            "GET",
            &format!("/slack/users?{query}"),
            None,
            None,
            None,
        )
        .await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-request");
    }
    let (code, headers, batch) = invoke(&app, "GET",
        "/slack/users?id=TBETA.U2&id=TALPHA.UADDED&id=TOTHER.U1&id=TBETA.UMISSING&id=TALPHA.UADDED&id=TALPHA.U1&id=TBETA.UBROKEN",
        None, None, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(batch["api_version"], "1");
    assert!(batch["next_cursor"].is_null());
    let ids: Vec<_> = batch["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["TBETA.U2", "TALPHA.UADDED", "TALPHA.U1"]);
    assert_eq!(batch["items"][1]["name"], "补查用户");
    let issues: Vec<_> = batch["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["id"].as_str().unwrap(),
                item["error"]["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        issues,
        [
            ("TOTHER.U1", "slack-project-not-found"),
            ("TBETA.UMISSING", "slack-user-not-found"),
            ("TBETA.UBROKEN", "slack-upstream-error")
        ]
    );
    assert_eq!(batch["issues"][1]["team_id"], "TBETA");

    let (code, _, cached) = invoke(
        &app,
        "GET",
        "/slack/users?id=TALPHA%2EUADDED",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(cached["items"][0]["name"], "补查用户");
    let mut requested = user_info_requests.lock().unwrap().clone();
    requested.sort_unstable();
    assert_eq!(
        requested,
        [
            ("Bearer xoxc-TALPHA".into(), "UADDED".into()),
            ("Bearer xoxc-TBETA".into(), "UBROKEN".into()),
            ("Bearer xoxc-TBETA".into(), "UMISSING".into())
        ]
    );

    let (_, _, first) = invoke(
        &app,
        "GET",
        "/slack/conversations?limit=1&kind=im",
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
        Some(beta),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(messages["items"][0]["text"], "Bearer xoxc-TBETA");
    assert_eq!(messages["items"][0]["author"]["id"], "TBETA.U2");
    let (code, _, error) = invoke(
        &app,
        "GET",
        "/slack/conversations/TBETA.DSHARED/messages",
        Some(alpha),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "slack-session-changed");

    for _ in 0..2 {
        let (status, _, _) =
            invoke(&app, "DELETE", "/data/slack/teams/TALPHA", None, None, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert_eq!(config["value"].as_array().unwrap().len(), 1);
    assert_eq!(config["value"][0]["team_id"], "TBETA");
    assert!(projects.service("TALPHA").await.is_err());
    assert!(projects.service("TBETA").await.is_ok());

    let pending_path = "/data/slack/teams/TPENDING";
    let created = app
        .clone()
        .oneshot(create("TPENDING", json!({})))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    assert_eq!(
        opened.try_recv().unwrap(),
        "http://127.0.0.1:47321/slack/team-setup/TPENDING"
    );
    let (status, _, partial) = invoke(
        &app,
        "PATCH",
        pending_path,
        None,
        None,
        Some(json!({"d_cookie":"test-web-cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        opened.try_recv().unwrap(),
        "http://127.0.0.1:47321/slack/team-setup/TPENDING"
    );
    let reference = partial["value"]["d_cookie"].as_str().unwrap();
    assert_eq!(
        passwords.fetch(reference).await.unwrap(),
        "\nd_cookie: test-web-cookie\n"
    );
    assert!(directory.0.join(reference).is_file());
    fs::remove_file(directory.0.join(reference)).unwrap();
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert_eq!(config["pending"][0]["value"], json!({"team_id":"TPENDING"}));
    let (status, _, retry) = invoke(&app, "PUT", pending_path, None, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(retry["status"], "pending");
    assert_eq!(
        opened.try_recv().unwrap(),
        "http://127.0.0.1:47321/slack/team-setup/TPENDING"
    );
    let (status, _, _) = invoke(&app, "DELETE", pending_path, None, None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!directory.0.join(reference).exists());
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert!(config["pending"].is_null());

    let team_path = "/data/slack/teams/TWEB";
    let created = app
        .clone()
        .oneshot(create("TWEB", json!({})))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    let created: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(created["status"], "pending");
    assert_eq!(created["value"], json!({"team_id":"TWEB"}));
    let setup_url = "http://127.0.0.1:47321/slack/team-setup/TWEB";
    assert_eq!(opened.try_recv().unwrap(), setup_url);
    let page = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/slack/team-setup/TWEB")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = to_bytes(page.into_body(), 1_048_576).await.unwrap();
    let html = std::str::from_utf8(&html).unwrap();
    for field in super::projects::SlackTeam::PASSWORD_FIELDS {
        assert!(html.contains(&format!("name=\"{field}\" type=\"password\"")));
    }
    assert_eq!(html.matches(r#"type="password""#).count(), 2);
    let (status, _, retry) = invoke(&app, "PUT", team_path, None, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(retry["status"], "pending");
    assert_eq!(opened.try_recv().unwrap(), setup_url);
    let (_, _, pending_before) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    let stored_before = fs::read(&config_path).unwrap();
    for token in [
        json!("not-a-token"),
        json!("xoxc-"),
        json!("xoxc-contains space"),
        json!(42),
    ] {
        let (status, _, error) = invoke(
            &app,
            "PATCH",
            team_path,
            None,
            None,
            Some(json!({"token":token})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid-config");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("xoxc-")
        );
    }
    assert!(opened.try_recv().is_err());
    let (_, _, pending_after) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert_eq!(pending_before, pending_after);
    assert_eq!(stored_before, fs::read(&config_path).unwrap());
    let pass_dir = directory.0.join("loki-assistant/slack");
    assert!(!fs::read_dir(pass_dir).unwrap().any(|file| {
        file.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("TWEB")
    }));
    let (status, _, error) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"token":"xoxc-TWEB","d_cookie":"test-web-cookie; Path=/"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "slack-invalid-web-cookie");
    let (status, _, staged) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"token":"xoxc-TWEB","d_cookie":"test-web-cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!staged.to_string().contains("xoxc-"));
    assert_eq!(staged["value"]["team_name"], "网页协作");
    assert_eq!(staged["value"]["user_id"], "U1");
    let (status, _, error) = invoke(&app, "PUT", team_path, None, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "config-pending-not-found");
    assert!(opened.try_recv().is_err());
    let (_, _, confirmed) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert!(confirmed["pending"].is_null());
    assert!(confirmed["value"].to_string().contains("TWEB"));
    let web_entry = confirmed["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|team| team["team_id"] == "TWEB")
        .unwrap();
    assert_eq!(web_entry["team_name"], "网页协作");
    assert_eq!(web_entry["user_id"], "U1");
    let (status, _, _) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"user_id":"UOTHER"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let password_path = web_entry["token"].as_str().unwrap();
    assert_eq!(password_path, "loki-assistant/slack/TWEB.gpg");
    assert_eq!(web_entry["d_cookie"], password_path);
    assert!(directory.0.join(password_path).exists());
    for (patch, expected_name) in [
        (json!({"name":"网页工作区"}), json!("网页工作区")),
        (json!({"name":null}), Value::Null),
    ] {
        let (status, _, response) = invoke(&app, "PATCH", team_path, None, None, Some(patch)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["value"]["name"], expected_name);
        assert_eq!(response["value"]["token"], password_path);
    }
    let (status, _, _) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"team_id":"TWEB"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let current_project = Arc::downgrade(&projects.service("TWEB").await.unwrap());
    let (status, _, response) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"d_cookie":"refreshed-web-cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response["value"]["token"], password_path);
    assert_eq!(response["value"]["d_cookie"], password_path);
    assert_eq!(
        passwords.fetch(password_path).await.unwrap(),
        "xoxc-TWEB\nd_cookie: refreshed-web-cookie\n"
    );
    timeout(Duration::from_secs(2), async {
        while current_project.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for (token, expected) in [
        ("xoxc-TWRONG", "xoxc-TWEB"),
        ("xoxc-TWEB-renewed", "xoxc-TWEB-renewed"),
    ] {
        let (status, _, staged) = invoke(
            &app,
            "PATCH",
            team_path,
            None,
            None,
            Some(json!({"token":token,"d_cookie":"refreshed-web-cookie"})),
        )
        .await;
        assert_eq!(
            status,
            if token == expected {
                StatusCode::OK
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            }
        );
        assert!(!staged.to_string().contains("xoxc-"));
        let [saved] = base
            .read_record_passwords("slack", "teams", "TWEB", ["token"])
            .await
            .unwrap();
        assert_eq!(saved.as_deref().map(str::trim_end), Some(expected));
        let (_, _, current) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
        if token == expected {
            assert!(current["pending"].is_null());
        } else {
            assert_eq!(current["pending"][0]["status"], "invalid");
            assert_eq!(
                current["pending"][0]["error"]["code"],
                "slack-team-mismatch"
            );
            assert!(current["pending"][0]["value"].get("token").is_none());
            assert!(current["pending"][0]["value"].get("d_cookie").is_none());
            assert_eq!(
                passwords.fetch(password_path).await.unwrap(),
                "xoxc-TWEB\nd_cookie: refreshed-web-cookie\n"
            );
            let pass_dir = directory.0.join("loki-assistant/slack");
            assert!(!fs::read_dir(pass_dir).unwrap().any(|file| {
                file.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("TWEB-")
            }));
            let restarted = ConfigService::load(&config_path).unwrap();
            let restarted_app = config::Module::from_service(restarted).router();
            let (_, _, after_restart) =
                invoke(&restarted_app, "GET", "/data/slack/teams", None, None, None).await;
            assert_eq!(after_restart["pending"], current["pending"]);
        }
    }
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    let password_path = config["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|team| team["team_id"] == "TWEB")
        .unwrap()["token"]
        .as_str()
        .unwrap();
    fs::remove_file(directory.0.join(password_path)).unwrap();
    for (path, pointer) in [
        ("/data/slack/teams", "/value"),
        ("/data/slack", "/values/teams/value"),
    ] {
        let (status, _, config) = invoke(&app, "GET", path, None, None, None).await;
        assert_eq!(status, StatusCode::OK);
        let team = config
            .pointer(pointer)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["team_id"] == "TWEB")
            .unwrap();
        assert!(team.get("token").is_none());
        assert!(team.get("d_cookie").is_none());
    }
    let (status, _, partial) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"token":"xoxc-TWEB"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert!(partial["value"].get("d_cookie").is_none());
    assert_eq!(opened.try_recv().unwrap(), setup_url);
    let (status, _, restored) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"d_cookie":"test-web-cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(restored["value"]["token"], password_path);
    assert_eq!(restored["value"]["d_cookie"], password_path);
    let (status, _, pending) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"d_cookie":null})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(opened.try_recv().unwrap(), setup_url);
    let staged = pending["value"]["token"].as_str().unwrap();
    let stage_dir = std::path::Path::new(staged).parent().unwrap();
    assert_eq!(
        stage_dir.parent().unwrap().to_str(),
        Some("loki-assistant/slack/pending")
    );
    assert!(uuid::Uuid::parse_str(stage_dir.file_name().unwrap().to_str().unwrap()).is_ok());
    assert_eq!(
        std::path::Path::new(staged).file_name().unwrap(),
        "TWEB.gpg"
    );
    assert!(directory.0.join(staged).is_file());
    assert!(directory.0.join(password_path).is_file());
    let (status, _, cleared) = invoke(
        &app,
        "PATCH",
        team_path,
        None,
        None,
        Some(json!({"token":null,"d_cookie":"test-web-cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["status"], "confirmed");
    assert_eq!(cleared["value"]["team_id"], "TWEB");
    assert!(cleared["value"].get("token").is_none());
    assert!(cleared["value"].get("d_cookie").is_none());
    assert!(!directory.0.join(staged).exists());
    assert!(!directory.0.join(password_path).exists());
    assert!(opened.try_recv().is_err());
    let (_, _, config) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
    assert!(config.get("pending").is_none());
    let team = config["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|team| team["team_id"] == "TWEB")
        .unwrap();
    assert!(team.get("token").is_none() && team.get("d_cookie").is_none());

    let (status, _, _) = invoke(&app, "DELETE", team_path, None, None, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    timeout(Duration::from_secs(2), async {
        while projects.service("TWEB").await.is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!directory.0.join(password_path).exists());

    projects.stop().await;
    worker.await.unwrap();
    upstream.abort();
}

#[test]
fn conversation_summary_tracks_unread_mentions_and_latest_reply() {
    use super::{
        protocol::{Identity, Message},
        store::{Account, Conversation},
    };

    let account = Account {
        identity: Identity {
            session_id: Uuid::nil(),
            team_id: "TTEAM".into(),
            user_id: "UME".into(),
            team_name: "工作区".into(),
        },
        access: String::new(),
        web_session: true,
        baseline: 100,
        imported_token_hash: None,
        revoked: false,
    };
    let mut room = Conversation::new("CCHAT", "public_channel", "产品讨论");
    room.participants = vec!["UME".into(), "UYOU".into()];
    for message in [
        json!({"ts":"101.0","user":"UYOU","text":"你好 <@UME>"}),
        json!({"ts":"102.0","user":"UME","text":"自己的消息 <@UME>"}),
        json!({"ts":"103.0","user":"UYOU","thread_ts":"101.0","text":"请看 <@UME|我的名字>"}),
        json!({"ts":"104.0","user":"UYOU","thread_ts":"101.0",
            "files":[{"title":"进度表","mimetype":"image/png","size":4096,
                      "permalink":"https://example.slack.com/files/F1",
                      "thumb_480":"https://files.slack.com/files-tmb/F1/report.png"}],
            "blocks":[{"type":"table","rows":[[{"type":"raw_text","text":"版本"}],
                [{"type":"rich_text","elements":[{"type":"rich_text_section",
                 "elements":[{"type":"text","text":"3.3.107"}]}]}]]}]}),
    ] {
        room.ingest(Message::from_slack(&message, None).unwrap());
    }
    let (_, _, summary) = room.summarize(&account);
    assert_eq!(summary["participant_ids"], json!(["TTEAM.UYOU"]));
    assert_eq!(summary["unread_count"], 3);
    assert_eq!(summary["last_unread_mention_ts"], "103.0");
    assert_eq!(summary["last_message"]["ts"], "104.0");
    assert_eq!(summary["last_message"]["author"]["id"], "TTEAM.UYOU");
    assert_eq!(summary["last_message"]["preview"], "附件 · 进度表");
    let message = &room.messages["104.0"];
    let wire = serde_json::to_value(message).unwrap();
    assert_eq!(wire["tables"], json!([[["版本"], ["3.3.107"]]]));
    assert_eq!(wire["attachments"][0]["image"], true);
    assert_eq!(wire["attachments"][0]["size"], 4096);
    assert!(wire["attachments"][0].get("image_url").is_none());

    room.read_positions
        .cursors
        .insert("101.0".into(), "104.0".into());
    let (_, _, summary) = room.summarize(&account);
    assert_eq!(summary["last_unread_mention_ts"], "101.0");
}

#[tokio::test]
async fn composition_mounts_flat_slack_routes() {
    let services = Arc::new(crate::Services::build());
    let app = crate::http::router(Arc::new(crate::runtime::RuntimeState::new(services)));
    let (status, _, meta) = invoke(&app, "GET", "/discover/modules/slack", None, None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(meta["apis"].as_array().unwrap().iter().any(|api| {
        api["method"] == "GET" && api["path"] == "/slack/conversations" && api["access"] == "local"
    }));
    let (status, _, _) = invoke(&app, "POST", "/slack/open", None, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(meta["apis"].as_array().unwrap().iter().all(|api| {
        let path = api["path"].as_str().unwrap();
        path != "/slack/status" && !path.starts_with("/slack/teams/")
    }));
}
