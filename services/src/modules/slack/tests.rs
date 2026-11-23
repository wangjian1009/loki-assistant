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
            assert!(script.contains("loki-assistant-dispatch"));
            if let Some((_, argument)) = script.split_once("(loki-assistant-dispatch-slack-messages ") {
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
                    .split_once("(loki-assistant-dispatch-slack-notification ")
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
struct Alerts {
    attempts: Mutex<Vec<NotificationContent>>,
    fail_next: AtomicBool,
}

impl NotificationLauncher for Alerts {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        self.attempts.lock().unwrap().push(content);
        let fail = self.fail_next.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                return Err("模拟通知转交失败".into());
            }
            Ok(())
        })
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

#[test]
fn moves_legacy_cache_with_read_positions_and_keeps_existing_target() {
    let directory = Directory::new();
    let legacy = directory.0.join("slack");
    let target = directory.0.join("assistant/slack");
    let room = "TTEAM/conversations/CWORK";
    fs::create_dir_all(legacy.join(room)).unwrap();
    fs::write(legacy.join(room).join("info.txt"), "原有已读位置").unwrap();

    super::migrate_legacy_cache(&directory.0).unwrap();
    assert!(!legacy.exists());
    let info = target.join(room).join("info.txt");
    assert_eq!(fs::read_to_string(&info).unwrap(), "原有已读位置");

    fs::create_dir(&legacy).unwrap();
    super::migrate_legacy_cache(&directory.0).unwrap();
    assert!(legacy.is_dir());
    assert_eq!(fs::read_to_string(&info).unwrap(), "原有已读位置");
}

struct Mock {
    socket: String,
    root: String,
    reply: String,
    sent: String,
    posts: AtomicUsize,
    misplaced_reply: AtomicBool,
    history_unavailable: AtomicBool,
}

fn alert_section(text: &str) -> Value {
    json!({"type":"section","text":{"type":"mrkdwn","text":text}})
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
            "blocks":[
                alert_section("&gt;组一 *IP绑定不通*\n&gt;已持续0.4小时"),
                {"type":"unsupported"},
                {"type":"rich_text","elements":[
                    {"type":"rich_text_section","elements":[{"type":"text","text":"普通正文"}]},
                    {"type":"rich_text_preformatted","language":"python","elements":[
                        {"type":"text","text":"  # "},
                        {"type":"link","url":"https://example.com/"},
                        {"type":"text","text":"\n  print(\"你好\")\n"}
                    ]},
                    {"type":"rich_text_preformatted","elements":[{"type":"text","text":"echo ok"}]}
                ]},
                {"type":"markdown","text":"```json\n{}\n```"},
                {"type":"table","rows":[[{"type":"raw_text","text":"正文表格"}]]},
                {"type":"section","text":{"type":"unknown","text":"无效正文"},
                 "fields":[{"type":"plain_text","text":"*字面量*"},{"type":"mrkdwn","text":" "}]},
                {"type":"section","text":{"type":"plain_text","text":" "}}
            ],
            "attachments":[{"title":"汇总","blocks":[
                {"type":"table","rows":[[{"type":"raw_text","text":"附件表格"}]]}
            ]}],
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
    assert_eq!(
        serde_json::to_value(saved).unwrap()["blocks"],
        json!([
            alert_section("&gt;组一 *IP绑定不通*\n&gt;已持续0.4小时"),
            {"type":"rich_text_preformatted","language":"python","text":"  # https://example.com/\n  print(\"你好\")\n"},
            {"type":"rich_text_preformatted","text":"echo ok"},
            {"type":"markdown","text":"```json\n{}\n```"},
            {"type":"table","rows":[["正文表格"]]},
            {"type":"section","fields":[{"type":"plain_text","text":"*字面量*"}]},
            {"type":"table","rows":[["附件表格"]]}
        ])
    );
    assert_eq!(
        serde_json::to_value(saved).unwrap()["tables"],
        json!([[["正文表格"]], [["附件表格"]]])
    );
    assert!(loaded.messages[&stale.ts].blocks.is_empty());
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

async fn wait_download(app: &Router, path: &str, session: Option<Uuid>, state: &str) -> Value {
    timeout(Duration::from_secs(3), async {
        loop {
            let (status, _, result) = invoke(app, "GET", path, session, None, None).await;
            assert_eq!(status, StatusCode::OK, "{result}");
            if result["state"] == state
                && (state != "downloading" || result["downloaded_bytes"] == 4)
            {
                break result;
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
        misplaced_reply: AtomicBool::default(),
        history_unavailable: AtomicBool::default(),
    });
    let media = get(|axum::extract::Path(name): axum::extract::Path<String>, headers: HeaderMap| async move {
            if name == "preview.png" {
                assert_eq!(headers["authorization"], "Bearer xoxc-test-user");
            } else {
                assert!(!headers.contains_key("authorization"));
            }
            let mut png = std::io::Cursor::new(Vec::new());
            image::DynamicImage::new_rgba8(1, 1)
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            ([("content-type", "image/png")], png.into_inner())
        });
    let interrupt_download = Arc::new(tokio::sync::Notify::new());
    let server_interrupt = interrupt_download.clone();
    let replace_original = Arc::new(AtomicBool::default());
    let server_replace = replace_original.clone();
    let original = get(move |headers: HeaderMap| {
        let interrupt = server_interrupt.clone();
        let replaced = server_replace.load(Ordering::SeqCst);
        async move {
            assert_eq!(headers["authorization"], "Bearer xoxc-test-user");
            assert_eq!(headers["accept-encoding"], "identity");
            let response = axum::response::Response::builder()
                .header("etag", "\"original-v1\"")
                .header("content-type", "image/png");
            if let Some(range) = headers.get("range") {
                assert_eq!(range, "bytes=4-");
                assert_eq!(headers["if-range"], "\"original-v1\"");
                if replaced {
                    return axum::response::Response::builder()
                        .header("etag", "\"original-v2\"")
                        .header("content-type", "image/png")
                        .body(Body::from("ijklmnop"))
                        .unwrap();
                }
                return response
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header("content-range", "bytes 4-7/8")
                    .body(Body::from("efgh"))
                    .unwrap();
            }
            let stream =
                futures_util::stream::unfold((0, interrupt), |(step, interrupt)| async move {
                    match step {
                        0 => Some((Ok(axum::body::Bytes::from_static(b"abcd")), (1, interrupt))),
                        1 => {
                            interrupt.notified().await;
                            Some((Err(std::io::Error::other("断开附件下载")), (2, interrupt)))
                        }
                        _ => None,
                    }
                });
            response
                .header("content-length", 8)
                .body(Body::from_stream(stream))
                .unwrap()
        }
    });
    let slack = Router::new()
        .route("/original-fixture", original)
        .route("/media/{name}", media.clone())
        .route("/slack-files2/bot_icons/{name}", media)
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
        .route("/users.list", get(|| async {
            Json(json!({"ok":true,"members":[
                {"id":"UYOU","name":"林舟","profile":{"image_48":"https://avatars.slack-edge.com/media/user.png"}}
            ]}))
        }))
        .route("/bots.info", get(|| async {
            Json(json!({"ok":true,"bot":{"id":"BNEWS","icons":{"image_48":"https://avatars.slack-edge.com/media/bot.png"}}}))
        }))
        .route("/conversations.history", get(|State(mock): State<Arc<Mock>>, Query(query): Query<std::collections::HashMap<String, String>>| async move {
            if mock.history_unavailable.load(Ordering::SeqCst) {
                return Json(json!({"ok":false,"error":"internal_error"}));
            }
            if query.get("channel").is_some_and(|channel| channel == "CWORK") {
                return Json(json!({"ok":true,"messages":[{"ts":"1.000001","text":"新闻","bot_id":"BNEWS",
                    "blocks":[alert_section("告警快照")]}]}));
            }
            let root = json!({"ts":mock.root,"text":"问候","user":"UYOU","reply_count":2,
                "blocks":[alert_section("历史告警"), {"type":"rich_text","elements":[
                    {"type":"rich_text_preformatted","language":"json",
                     "elements":[{"type":"text","text":"{}"}]}
                ]}]});
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
                json!({"ok":true,"messages":[{"ts":mock.reply,"thread_ts":mock.root,"text":"线程回复","user":"UYOU",
                    "blocks":[alert_section("线程告警")]}]})
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
            assert_eq!(body["reply_broadcast"], false);
            assert_eq!(body["text"], "收到");
            mock.posts.fetch_add(1, Ordering::SeqCst);
            if mock.misplaced_reply.load(Ordering::SeqCst) {
                let ts = format!("{}1", mock.sent);
                return Json(json!({"ok":true,"ts":ts,"channel":"DDIRECT",
                    "message":{"ts":ts,"thread_ts":ts,"text":"收到"}}));
            }
            Json(json!({"ok":true,"ts":mock.sent,"message":{"ts":mock.sent,"thread_ts":mock.root,
                        "text":"收到"}}))
        })).with_state(Arc::clone(&mock));
    let http = tokio::spawn(async {
        axum::serve(web, slack).await.unwrap();
    });
    let (done, messages_sent) = tokio::sync::oneshot::channel();
    let (send_channel, channel_ready) = tokio::sync::oneshot::channel();
    let (watch_events, mut watched_socket_events) = tokio::sync::mpsc::unbounded_channel::<Value>();
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
            "user":"UYOU","text":"<!channel> 请 <@UMEOTHER> 看一下",
            "blocks":[alert_section("&gt;实时告警 `节点记录冲突`")],
            "icons":{"image_48":"https://s3-us-west-2.amazonaws.com/slack-files2/bot_icons/alert.png"},
            "files":[{"id":"FPREVIEW","name":"截图.png","mimetype":"image/png",
                       "thumb_480":"https://files.slack.com/media/preview.png",
                       "url_private_download":"https://files.slack.com/original-fixture"}]});
        let mention = json!({"type":"message","channel":"CWORK","ts":socket_mock.reply,
            "thread_ts":socket_mock.root,"user":"UYOU","text":"请 <@UME> 看一下"});
        for payload in [&ordinary, &mention, &mention] {
            stream
                .send(Message::Text(payload.to_string().into()))
                .await
                .unwrap();
        }
        let placeholder = json!({"type":"message","subtype":"bot_message","channel":"CWORK",
            "ts":socket_mock.sent,"thread_ts":socket_mock.root,"bot_id":"BNEWS","text":"思考中..."});
        let answer = json!({"type":"message","subtype":"message_changed","channel":"CWORK",
            "ts":socket_mock.sent, "previous_message":placeholder,
            "message":{"ts":socket_mock.sent,"bot_id":"BNEWS","text":"正式回复"}});
        for payload in [&placeholder, &answer] {
            stream
                .send(Message::Text(payload.to_string().into()))
                .await
                .unwrap();
        }
        while let Some(event) = watched_socket_events.recv().await {
            stream
                .send(Message::Text(event.to_string().into()))
                .await
                .unwrap();
        }
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
    let avatar_dir = directory.0.join("cache/TTEAM/users");
    timeout(Duration::from_secs(3), async {
        while !(avatar_dir.join("UYOU.png").is_file() && avatar_dir.join("BNEWS.png").is_file()) {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "启动头像预热，用户 {}，机器人 {}",
            avatar_dir.join("UYOU.png").is_file(),
            avatar_dir.join("BNEWS.png").is_file()
        )
    });
    let (_, _, users) = invoke(&app, "GET", "/slack/users?id=TTEAM.UYOU", None, None, None).await;
    assert_eq!(
        users["items"][0]["avatar_path"],
        json!(avatar_dir.join("UYOU.png"))
    );
    timeout(Duration::from_secs(3), async {
        loop {
            let (_, _, picker) =
                invoke(&app, "GET", "/slack/conversations/picker", None, None, None).await;
            if picker["items"].as_array().unwrap().iter().any(|item| {
                item["id"] == "TTEAM.DDIRECT"
                    && item["participants"][0]["avatar_path"] == json!(avatar_dir.join("UYOU.png"))
            }) {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("会话选择器应提供本机头像路径");
    let (status_code, headers, _) =
        invoke(&app, "GET", "/slack/conversations", None, None, None).await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    timeout(Duration::from_secs(5), messages_sent)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while alerts.attempts.lock().unwrap().is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(alerts.attempts.lock().unwrap().len(), 1);
    assert_eq!(alerts.attempts.lock().unwrap()[0].app.as_deref(), Some("slack"));
    let navigation = alerts.attempts.lock().unwrap()[0].navigation_url.clone().unwrap();
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
    let read_status = || async {
        invoke(&app, "GET", "/status", None, None, None).await.2["status"]["slack"]
            ["new_conversations"]
            .clone()
    };
    let conversations = read_status().await;
    assert_eq!(conversations.as_array().unwrap().len(), 1);
    assert_eq!(conversations[0]["id"], "TTEAM.DDIRECT");
    assert_eq!(conversations[0]["unread_count"], 2);
    assert_eq!(
        conversations[0]["callback"],
        format!("/slack/conversations/TTEAM.DDIRECT/open?message_ts={}&thread_ts={}", mock.reply, mock.root)
    );
    assert_eq!(conversations[0]["team_name"], "工作区");
    assert_eq!(conversations[0]["session_id"], json!(session));

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
    assert_eq!(
        view["items"][0]["blocks"],
        json!([
            alert_section("历史告警"),
            {"type":"rich_text_preformatted","language":"json","text":"{}"}
        ])
    );
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
    assert_eq!(
        view["items"][0]["blocks"],
        json!([alert_section("线程告警")])
    );
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
    let cached_reply = mock.reply.clone();
    service
        .update_cache(session.unwrap(), move |cache, _| {
            assert!(cache.conversations.get_mut("DDIRECT").unwrap().messages.remove(&cached_reply).is_some());
            Ok(())
        })
        .await
        .unwrap();
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
    assert_eq!(
        emacs.deliveries.lock().unwrap()[0]["snapshot"]["items"][0]["blocks"],
        json!([alert_section("告警快照")])
    );

    send_channel.send(()).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let delivered = emacs.deliveries.lock().unwrap().iter().any(|batch| {
                batch["kind"] == "changes"
                    && batch["items"].as_array().unwrap().iter().any(|item| {
                        item["ts"] == mock.root
                            && item["author"]["id"] == "TTEAM.UYOU"
                            && item["reply_count"].as_u64().is_some_and(|count| count >= 1)
                    })
            });
            let edited = emacs.deliveries.lock().unwrap().iter().any(|batch| {
                batch["thread_items"].as_array().is_some_and(|items| {
                    items.iter().any(|item| {
                        item["ts"] == mock.sent
                            && item["thread_ts"] == mock.root
                            && item["text"] == "正式回复"
                            && item["author"]["id"] == "TTEAM.BNEWS"
                    })
                })
            });
            if delivered && edited {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let delivered = emacs.deliveries.lock().unwrap();
    let first = delivered
        .iter()
        .filter_map(|batch| batch["items"].as_array())
        .flatten()
        .find(|item| item["ts"] == mock.root)
        .unwrap();
    assert_eq!(
        first["blocks"],
        json!([alert_section("&gt;实时告警 `节点记录冲突`")])
    );
    assert_eq!(
        first["author"]["avatar_path"],
        json!(avatar_dir.join("UYOU.png"))
    );
    let preview_path = directory
        .0
        .join("cache/TTEAM/attachments/FPREVIEW.preview.png");
    assert_eq!(first["attachments"][0]["preview_path"], json!(preview_path));
    assert!(preview_path.is_file());
    let icon = std::path::Path::new(first["icon_path"].as_str().unwrap());
    assert!(icon.starts_with(directory.0.join("cache/TTEAM/icons")) && icon.is_file());
    assert!(first.get("icon_url").is_none());
    drop(delivered);

    let original_path = format!(
        "/slack/conversations/TTEAM.CWORK/messages/{}/attachments/0/original",
        mock.root
    );
    let (status, _, created) = invoke(&app, "POST", &original_path, session, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    assert_eq!(created.as_object().unwrap().len(), 1);
    let download_id = created["download_id"].as_str().unwrap();
    let download_path = format!("/slack/downloads/{download_id}");
    let progress = wait_download(&app, &download_path, session, "downloading").await;
    assert_eq!(progress["total_bytes"], 8);
    assert!(progress.get("cache_path").is_none());
    assert_eq!(
        invoke(&app, "POST", &original_path, session, None, None)
            .await
            .2,
        created
    );
    interrupt_download.notify_one();
    let failed = wait_download(&app, &download_path, session, "failed").await;
    assert_eq!(failed["error"]["code"], "slack-data-unavailable");
    assert_eq!(failed["downloaded_bytes"], 4);
    assert!(failed.get("cache_path").is_none());
    let cached_original = directory
        .0
        .join("cache/TTEAM/attachments/FPREVIEW.original.png");
    assert!(!cached_original.exists());
    let sent = mock.sent.clone();
    let root = mock.root.clone();
    service
        .update_cache(session.unwrap(), move |cache, _| {
            cache
                .conversations
                .get_mut("CWORK")
                .unwrap()
                .remove(&sent, Some(&root));
            Ok(())
        })
        .await
        .unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            if emacs.deliveries.lock().unwrap().iter().any(|batch| {
                batch["deleted_thread_messages"]
                    .as_array()
                    .is_some_and(|items| {
                        items.contains(&json!({"ts":mock.sent, "thread_ts":mock.root}))
                    })
            }) {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
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
            if let Some(alert) = alerts.attempts.lock().unwrap().get(1) {
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
    let conversations = read_status().await;
    assert_eq!(conversations.as_array().unwrap().len(), 2);
    assert_eq!(conversations[0]["id"], "TTEAM.CWORK");
    assert_eq!(conversations[0]["title"], "产品讨论");
    assert_eq!(conversations[0]["callback"], callback.as_ref());
    assert_eq!(conversations[1]["id"], "TTEAM.DDIRECT");

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
    {
        let state = service.state.lock().await;
        let room = &state.cache.conversations["DDIRECT"];
        let message = &room.messages[&mock.sent];
        assert_eq!(message.author.as_ref().unwrap().id, "UME");
    }
    assert_eq!(alerts.attempts.lock().unwrap().len(), 2);
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
    mock.misplaced_reply.store(true, Ordering::SeqCst);
    let misplaced_key = Some(Uuid::new_v4());
    let (status, headers, _) =
        invoke(&app, "POST", &thread, session, misplaced_key, Some(body.clone())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let location = headers["location"].to_str().unwrap();
    let receipt = timeout(Duration::from_secs(3), async {
        loop {
            let (_, _, receipt) = invoke(&app, "GET", location, session, None, None).await;
            if receipt["state"] != "pending" {
                break receipt;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(receipt["state"], "unknown");
    assert_eq!(receipt["error"]["code"], "slack-delivery-target-mismatch");
    let (_, _, repeated) = invoke(&app, "POST", &thread, session, misplaced_key, Some(body)).await;
    assert_eq!(repeated["state"], "unknown");
    assert_eq!(mock.posts.load(Ordering::SeqCst), 2);

    let watch_path = "/slack/conversations/TTEAM.CWORK/watch";
    let list_path = "/slack/watches";
    assert_eq!(
        invoke(&app, "GET", watch_path, None, None, None).await.2["watching"],
        false
    );
    assert_eq!(
        invoke(
            &app,
            "PUT",
            "/slack/conversations/TTEAM.DDIRECT/watch",
            None,
            None,
            Some(json!({"watching":true}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    for _ in 0..2 {
        let (status, headers, body) = invoke(
            &app,
            "PUT",
            watch_path,
            None,
            None,
            Some(json!({"watching":true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(body["watching"], true);
    }
    assert_eq!(
        invoke(&app, "GET", list_path, None, None, None).await.2,
        json!({"api_version":"1","items":[{"conversation_id":"TTEAM.CWORK"}]})
    );
    let reopened = ConfigService::load(directory.0.join("config.yml")).unwrap();
    assert_eq!(
        reopened.value("slack", "watches").await.unwrap().value,
        json!([{"id":"TTEAM.CWORK","display_name":"#产品讨论"}])
    );
    let top = format!("{now}.100001");
    let reply = format!("{now}.100002");
    let self_reply = format!("{now}.100003");
    let mention_ts = format!("{now}.100004");
    for event in [
        json!({"type":"message","channel":"CWORK","ts":top,"user":"UYOU","text":"普通消息"}),
        json!({"type":"message","channel":"CWORK","ts":reply,"thread_ts":"1.0",
               "user":"UYOU","text":"未打开线索的普通回复",
               "blocks":[alert_section("&gt;请 <@UYOU> 检查 *<https://example.com/alerts|告警明细>*")]}),
        json!({"type":"message","subtype":"thread_broadcast","channel":"CWORK","ts":reply,
               "event_ts":format!("{now}.200002"),"root":{"ts":"1.0"},"user":"UYOU","text":"同一条回复"}),
        json!({"type":"message","channel":"CWORK","ts":self_reply,"thread_ts":"1.0",
               "user":"UME","text":"自己的回复"}),
        json!({"type":"message","channel":"CWORK","ts":mention_ts,"user":"UYOU","text":"<@UME>"}),
    ] {
        watch_events.send(event).unwrap();
    }
    timeout(Duration::from_secs(3), async {
        while alerts.attempts.lock().unwrap().len() < 5 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        alerts.attempts.lock().unwrap()[3].body.as_deref(),
        Some("林舟 · 请 @林舟 检查 告警明细")
    );
    let callbacks: Vec<_> = alerts
        .attempts
        .lock()
        .unwrap()
        .iter()
        .skip(2)
        .map(|alert| {
            url::Url::parse(alert.navigation_url.as_ref().unwrap())
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "callback")
                .unwrap()
                .1
                .into_owned()
        })
        .collect();
    assert_eq!(
        callbacks,
        [
            format!("/slack/conversations/TTEAM.CWORK/open?message_ts={top}"),
            format!("/slack/conversations/TTEAM.CWORK/open?message_ts={reply}&thread_ts=1.0"),
            format!("/slack/conversations/TTEAM.CWORK/open?message_ts={mention_ts}"),
        ]
    );
    let before_unwatch = read_status().await;
    let disabled = invoke(
        &app,
        "PUT",
        watch_path,
        None,
        None,
        Some(json!({"watching":false})),
    )
    .await;
    assert_eq!(disabled.0, StatusCode::OK, "{}", disabled.2);
    assert_eq!(disabled.2["watching"], false);
    assert_eq!(
        invoke(&app, "GET", list_path, None, None, None).await.2["items"],
        json!([])
    );
    assert_eq!(read_status().await, before_unwatch);
    for event in [
        json!({"type":"message","channel":"CWORK","ts":format!("{now}.300001"),
               "user":"UYOU","text":"取消关注后的普通消息"}),
        json!({"type":"message","channel":"CWORK","ts":format!("{now}.300002"),
               "user":"UYOU","text":"<@UME>"}),
    ] {
        watch_events.send(event).unwrap();
    }
    timeout(Duration::from_secs(3), async {
        loop {
            let alerts = alerts.attempts.lock().unwrap();
            assert!(alerts.len() <= 6);
            if let Some(alert) = alerts.get(5) {
                assert!(alert.navigation_url.as_ref().unwrap().contains("300002"));
                break;
            }
            drop(alerts);
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    alerts.fail_next.store(true, Ordering::SeqCst);
    let retry_ts = format!("{now}.300003");
    watch_events
        .send(json!({"type":"message","channel":"CWORK","ts":retry_ts,
                     "user":"UYOU","text":"<@UME>",
                     "blocks":[alert_section("*原始告警*")]}))
        .unwrap();
    timeout(Duration::from_secs(3), async {
        while alerts.attempts.lock().unwrap().len() < 7 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    watch_events
        .send(json!({"type":"message","subtype":"message_changed","channel":"CWORK",
                     "message":{"ts":retry_ts,"user":"UYOU","text":"告警已更新"}}))
        .unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            if service
                .state
                .lock()
                .await
                .cache
                .conversations
                .get("CWORK")
                .and_then(|room| room.messages.get(&retry_ts))
                .is_some_and(|message| message.text == "告警已更新")
            {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let submitted: Vec<_> = alerts.attempts.lock().unwrap()[..6]
        .iter()
        .map(|alert| alert.notification_id.clone())
        .collect();
    for id in submitted {
        notification
            .receive_receipt(
                &id,
                crate::modules::notification::protocol::NotificationReceipt::Submitted {},
            )
            .await
            .unwrap();
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    let retry_worker = tokio::spawn({
        let notification = Arc::clone(&notification);
        async move { notification.retry_pending().await }
    });
    tokio::time::resume();
    timeout(Duration::from_secs(3), async {
        while alerts.attempts.lock().unwrap().len() < 8 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    {
        let attempts = alerts.attempts.lock().unwrap();
        assert_eq!(attempts[6].body.as_deref(), Some("林舟 · 原始告警"));
        assert_eq!(attempts[6].notification_id, attempts[7].notification_id);
        assert_eq!(attempts[6].body, attempts[7].body);
        assert_eq!(attempts[6].navigation_url, attempts[7].navigation_url);
    }
    retry_worker.abort();
    let _ = retry_worker.await;
    assert_eq!(
        read_status().await[0]["callback"],
        format!("/slack/conversations/TTEAM.CWORK/open?message_ts={retry_ts}")
    );
    let (code, _, _) = invoke(
        &app,
        "PUT",
        "/slack/conversations/TTEAM.CWORK/read-position",
        session,
        None,
        Some(json!({"through_ts":retry_ts})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    // 顶层已读后，尚未阅读的线程仍列在托盘中。
    let unread = read_status().await;
    let channel = unread.as_array().unwrap().iter().find(|row| row["id"] == "TTEAM.CWORK").unwrap();
    assert!(channel["callback"].as_str().unwrap().contains("&thread_ts="));
    let (code, _, _) = invoke(
        &app,
        "PUT",
        &format!("/slack/conversations/TTEAM.CWORK/threads/{}/read-position", mock.root),
        session,
        None,
        Some(json!({"through_ts":mock.reply})),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    // 另一条线程仍有未读消息，推进对应线程的已读位置后移除。
    let unread = read_status().await;
    let channel = unread.as_array().unwrap().iter().find(|row| row["id"] == "TTEAM.CWORK").unwrap();
    assert_eq!(channel["callback"], format!("/slack/conversations/TTEAM.CWORK/open?message_ts={reply}&thread_ts=1.0"));
    let (code, _, _) = invoke(
        &app, "PUT", "/slack/conversations/TTEAM.CWORK/threads/1.0/read-position",
        session, None, Some(json!({"through_ts":reply})),
    ).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(read_status().await, json!([]));
    let cached_unread = format!("{now}.400000");
    let saved = cached_unread.clone();
    service.update_cache(session.unwrap(), move |cache, _| {
        let message = super::protocol::Message::from_slack(
            &json!({"ts":saved,"text":"历史普通消息","user":"UYOU"}), None
        ).unwrap();
        cache.conversations.get_mut("CWORK").unwrap().ingest(message);
        Ok(())
    }).await.unwrap();
    assert_eq!(read_status().await[0]["unread_count"], 1);
    projects.stop().await;
    worker.await.unwrap();

    mock.history_unavailable.store(true, Ordering::SeqCst);
    let (projects, app, worker) = start();
    let service = ready_service(&projects, "TTEAM").await;
    let fresh_session = service.session_id().await;
    assert_ne!(fresh_session, session);
    let unread_after_restart = invoke(&app, "GET", "/status/slack/new_conversations", None, None, None).await.2;
    assert_eq!(unread_after_restart["value"][0]["id"], "TTEAM.CWORK");
    assert_eq!(unread_after_restart["value"][0]["unread_count"], 1);
    assert_eq!(unread_after_restart["value"][0]["callback"],
        format!("/slack/conversations/TTEAM.CWORK/open?message_ts={cached_unread}"));

    let (status, _, error) = invoke(&app, "GET", &download_path, fresh_session, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"]["code"], "slack-download-not-found");
    let (status, _, resumed) =
        invoke(&app, "POST", &original_path, fresh_session, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    let resumed_id = resumed["download_id"].as_str().unwrap();
    assert_ne!(resumed_id, download_id);
    let resumed_path = format!("/slack/downloads/{resumed_id}");
    let completed = wait_download(&app, &resumed_path, fresh_session, "completed").await;
    assert_eq!(completed["downloaded_bytes"], 8);
    assert_eq!(completed["total_bytes"], 8);
    assert_eq!(completed["cache_path"], json!(cached_original));
    assert_eq!(fs::read(&cached_original).unwrap(), b"abcdefgh");
    let (status, _, cached) = invoke(&app, "POST", &original_path, fresh_session, None, None).await;
    assert_eq!(status, StatusCode::OK, "{cached}");
    let cache_task = format!(
        "/slack/downloads/{}",
        cached["download_id"].as_str().unwrap()
    );
    let completed = wait_download(&app, &cache_task, fresh_session, "completed").await;
    assert_eq!(completed["cache_path"], json!(cached_original));
    let (status, _, error) = invoke(&app, "GET", &cache_task, session, None, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "slack-session-changed");

    fs::remove_file(&cached_original).unwrap();
    let (_, _, created) = invoke(&app, "POST", &original_path, fresh_session, None, None).await;
    let download = format!("/slack/downloads/{}", created["download_id"].as_str().unwrap());
    wait_download(&app, &download, fresh_session, "downloading").await;
    interrupt_download.notify_one();
    wait_download(&app, &download, fresh_session, "failed").await;
    replace_original.store(true, Ordering::SeqCst);
    let (_, _, retried) = invoke(&app, "POST", &original_path, fresh_session, None, None).await;
    let retried = format!("/slack/downloads/{}", retried["download_id"].as_str().unwrap());
    wait_download(&app, &retried, fresh_session, "completed").await;
    assert_eq!(fs::read(&cached_original).unwrap(), b"ijklmnop");

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
                    && batch["snapshot"]["thread_items"]
                        .as_array()
                        .is_some_and(|items| {
                            items.iter().any(|item| {
                                item["ts"] == mock.sent && item["thread_ts"] == mock.root
                            })
                        })
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
            if stdout.starts_with("模拟解密失败".as_bytes()) {
                return Ok(crate::modules::password::service::CommandOutput {
                    status: Some(1),
                    stdout: Vec::new(),
                });
            }
            Ok(crate::modules::password::service::CommandOutput {
                status: Some(0),
                stdout,
            })
        })
    }
}

#[tokio::test]
async fn startup_credential_notifications_use_config_reset_and_team_setup() {
    let directory = Directory::new();
    let (config, passwords) = password_config(&directory.0.join("config.yml"), &directory.0).await;
    let invalid_path = "loki-assistant/slack/TINVALID.gpg";
    let missing_path = "loki-assistant/slack/TMISSING.gpg";
    let unreadable_path = "loki-assistant/slack/TUNREADABLE.gpg";
    for (team, path, entry) in [
        ("TINVALID", invalid_path, "xoxc-expired\nd_cookie: cookie"),
        ("TMISSING", missing_path, "xoxc-missing-cookie"),
        ("TUNREADABLE", unreadable_path, "模拟解密失败"),
    ] {
        passwords.save_multiline(path, entry).await.unwrap();
        config
            .replace_record(
                "slack".into(),
                "teams".into(),
                team.into(),
                json!({"token":path,"d_cookie":path}),
                true,
                true,
            )
            .await
            .unwrap();
    }
    // 启动后从持久化配置加载仍存在的凭据引用。
    let (config, _) = password_config(&directory.0.join("config.yml"), &directory.0).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let expired_auth_checks = Arc::new(AtomicUsize::new(0));
    let upstream_checks = Arc::clone(&expired_auth_checks);
    let upstream = tokio::spawn(async move {
        let app = Router::new().route(
            "/auth.test",
            post(move |headers: HeaderMap| {
                let checks = Arc::clone(&upstream_checks);
                async move {
                    Json(match headers["authorization"].to_str().unwrap() {
                        "Bearer xoxc-expired" => {
                            checks.fetch_add(1, Ordering::SeqCst);
                            json!({"ok":false,"error":"invalid_auth"})
                        }
                        "Bearer xoxc-renewed" => {
                            json!({"ok":true,"team_id":"TINVALID","team":"工作区","user_id":"U1"})
                        }
                        _ => panic!("仅应校验完整可读的凭据"),
                    })
                }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    let alerts = Arc::new(Alerts::default());
    let notification = Arc::new(NotificationService::with_launcher(
        alerts.clone(),
        Duration::from_secs(1),
    ));
    let status = Arc::new(StatusService::new());
    let (browser, mut opened) = tokio::sync::mpsc::unbounded_channel();
    let projects = Arc::new(Projects::with_backend(
        config.clone(),
        notification.clone(),
        status.clone(),
        crate::modules::emacs::Module::new(config.clone(), notification, status.clone()).service(),
        Backend {
            api,
            cache_root: None,
            open_browser: Arc::new(move |url| {
                let browser = browser.clone();
                Box::pin(async move {
                    browser.send(url).unwrap();
                    Ok(())
                })
            }),
        },
    ));
    let config_module = crate::modules::config::Module::from_service(
        (*config)
            .clone()
            .with_change_handler("slack", projects.clone()),
    );
    let notification_router = Arc::new(super::routes::config_notification_router(projects.clone()));
    config_module
        .service()
        .attach_notifications(&notification_router);
    let app = projects
        .router()
        .merge(status.router())
        .merge(config_module.router());
    let worker = tokio::spawn(Arc::clone(&projects).run(47321));

    let states = timeout(Duration::from_secs(3), async {
        loop {
            let (_, _, response) =
                invoke(&app, "GET", "/status/slack/connections", None, None, None).await;
            if let Some(states) = response["value"].as_array()
                && states.len() == 3
                && states.iter().all(|state| state["issue_code"].is_string())
                && alerts.attempts.lock().unwrap().len() == 2
            {
                break states.clone();
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for (state, (team, code)) in states.iter().zip([
        ("TINVALID", "slack-authorization-required"),
        ("TMISSING", "slack-web-session-incomplete"),
        ("TUNREADABLE", "slack-password-unavailable"),
    ]) {
        assert_eq!(state["team_id"], team);
        assert_eq!(state["issue_code"], code);
    }
    assert_eq!(states[0]["authorization_state"], "reauthorization_required");
    assert_eq!(states[1]["authorization_state"], "reauthorization_required");
    assert_eq!(states[2]["authorization_state"], "unconfigured");
    assert!(opened.try_recv().is_err());
    assert!(directory.0.join(invalid_path).is_file());
    assert!(directory.0.join(missing_path).is_file());

    let mut callbacks: Vec<_> = alerts
        .attempts
        .lock()
        .unwrap()
        .iter()
        .map(|alert| {
            assert_eq!(alert.title, "Slack 需要重新授权");
            url::Url::parse(alert.navigation_url.as_deref().unwrap())
                .unwrap()
                .query_pairs()
                .find(|(key, _)| key == "callback")
                .unwrap()
                .1
                .into_owned()
        })
        .collect();
    callbacks.sort();
    for (callback, team, path) in [
        (&callbacks[0], "TINVALID", invalid_path),
        (&callbacks[1], "TMISSING", missing_path),
    ] {
        assert_eq!(callback, &format!("/slack/teams/{team}/reset-credentials"));
        let (status, _, response) = invoke(&app, "POST", callback, None, None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response, json!({"api_version":"1","team_id":team}));
        let setup = timeout(Duration::from_secs(2), opened.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            setup,
            format!("http://127.0.0.1:47321/slack/team-setup/{team}")
        );
        let (_, _, response) = invoke(&app, "GET", "/data/slack/teams", None, None, None).await;
        let record = response["value"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["team_id"] == team)
            .unwrap();
        assert!(record.get("token").is_none() && record.get("d_cookie").is_none());
        assert!(!directory.0.join(path).exists());
    }
    assert_eq!(expired_auth_checks.load(Ordering::SeqCst), 1);

    let (status, _, response) = invoke(
        &app,
        "POST",
        "/slack/teams/TUNREADABLE/reset-credentials",
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response["error"]["code"], "slack-password-unavailable");
    assert!(directory.0.join(unreadable_path).is_file());

    let (status, _, _) = invoke(
        &app,
        "PATCH",
        "/data/slack/teams/TINVALID",
        None,
        None,
        Some(json!({"token":"xoxc-renewed","d_cookie":"cookie"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, response) = invoke(&app, "POST", &callbacks[0], None, None, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["error"]["code"], "slack-credentials-current");
    assert!(directory.0.join(invalid_path).is_file());
    assert!(opened.try_recv().is_err());

    projects.stop().await;
    worker.await.unwrap();
    upstream.abort();
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
    let dm_open_requests = Arc::new(AtomicUsize::new(0));
    let opened_dms = Arc::clone(&dm_open_requests);
    let slack = Router::new()
        .route(
            "/conversations.open",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let opened_dms = Arc::clone(&opened_dms);
                async move {
                    assert_eq!(headers["authorization"], "Bearer xoxc-TALPHA");
                    assert_eq!(headers["cookie"], "d=test-web-cookie");
                    assert_eq!(body, json!({"users":"UADDED"}));
                    opened_dms.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"ok":true,"channel":{"id":"DNEW"}}))
                }
            }),
        )
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
    assert_eq!(items.len(), 4);
    let watches = items.iter().find(|item| item["key"] == "watches").unwrap();
    assert_eq!(watches["read_only"], true);
    assert_eq!(watches["structure"]["type_name"], "SlackWatch");
    let (_, _, blocked) = invoke(
        &app,
        "PUT",
        "/data/slack/watches",
        None,
        None,
        Some(json!([])),
    )
    .await;
    assert_eq!(blocked["error"]["code"], "config-read-only");
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
    assert!(im["participants"][0].get("avatar_url").is_none());
    assert!(
        im["search_terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|term| term == "lz")
    );
    let (code, headers, users) = invoke(&app, "GET", "/slack/users", None, None, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(users["items"][0]["id"], "TALPHA.U1");
    assert_eq!(users["items"][0]["name"], "林舟");
    assert_eq!(users["items"][0]["username"], "linzhou");
    assert!(users["items"][0].get("avatar_url").is_none());
    assert_eq!(users["items"][1]["id"], "TALPHA.U2");
    assert_eq!(users["items"][2]["id"], "TBETA.U1");
    assert_eq!(users["items"][3]["id"], "TBETA.U2");
    assert_eq!(users["items"].as_array().unwrap().len(), 4);
    assert!(users.get("next_cursor").is_none());

    let nickname_path = "/slack/users/TALPHA.U2/nickname";
    for malformed in [json!({}), json!({"nickname":42}), json!({"nickname":"两行\n名字"})] {
        let status = invoke(&app, "PUT", nickname_path, None, None, Some(malformed)).await.0;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (code, _, set) = invoke(&app, "PUT", nickname_path, None, None,
        Some(json!({"nickname":" 小林 "}))).await;
    assert_eq!(code, StatusCode::OK, "{set}");
    assert_eq!(set["user"]["name"], "林舟");
    assert_eq!(set["user"]["nickname"], "小林");
    let persisted = ConfigService::load(&config_path).unwrap().value("slack", "nicknames").await.unwrap();
    assert_eq!(persisted.value, json!([{"id":"TALPHA.U2","nickname":"小林"}]));
    let users = invoke(&app, "GET", "/slack/users?id=TALPHA.U2&id=TBETA.U2", None, None, None).await.2;
    assert_eq!(users["items"][0]["nickname"], "小林");
    assert!(users["items"][1]["nickname"].is_null());
    let timeline = invoke(&app, "GET", "/slack/conversations/TALPHA.DSHARED/messages", None, None, None).await.2;
    assert_eq!(timeline["items"][0]["author"]["nickname"], "小林");
    timeout(Duration::from_secs(3), async {
        loop {
            let picker = invoke(&app, "GET", "/slack/conversations/picker", None, None, None).await.2;
            if let Some(room) = picker["items"].as_array().unwrap().iter()
                .find(|room| room["id"] == "TALPHA.DSHARED" && room["title"] == "小林") {
                assert_eq!(room["participants"][0]["nickname"], "小林");
                assert_eq!(room["last_message"]["author_name"], "小林");
                assert!(room["search_terms"].as_array().unwrap().iter().any(|term| term == "小林"));
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let (code, _, cleared) = invoke(&app, "PUT", nickname_path, None, None,
        Some(json!({"nickname":null}))).await;
    assert_eq!(code, StatusCode::OK);
    assert!(cleared["user"]["nickname"].is_null());


    for query in [
        "id=".to_owned(),
        "id=U1".to_owned(),
        "id=TALPHA.U1&id=wrong".to_owned(),
        "limit=1".to_owned(),
        "cursor=page".to_owned(),
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
    assert!(batch.get("next_cursor").is_none());
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

    let (code, headers, users) = invoke(&app, "GET", "/slack/users", None, None, None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    let users = users["items"].as_array().unwrap();
    assert_eq!(users.len(), 5);
    let peer = users.iter().find(|item| item["id"] == "TBETA.U2").unwrap();
    assert_eq!(peer["team_name"], "网页协作");
    assert_eq!(peer["session_id"], json!(beta));
    assert!(peer.get("avatar_url").is_none());

    let (code, _, dm) = invoke(
        &app,
        "POST",
        "/slack/users/TBETA.U2/dm",
        Some(beta),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(dm["conversation"]["id"], "TBETA.DSHARED");
    assert_eq!(dm["conversation"]["title"], "林舟");
    assert_eq!(dm_open_requests.load(Ordering::SeqCst), 0);

    let dm_path = "/slack/users/TALPHA.UADDED/dm";
    let (code, _, error) = invoke(&app, "POST", dm_path, Some(beta), None, None).await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "slack-session-changed");
    let (left, right) = tokio::join!(
        invoke(&app, "POST", dm_path, Some(alpha), None, None),
        invoke(&app, "POST", dm_path, Some(alpha), None, None)
    );
    for (code, _, dm) in [left, right] {
        assert_eq!(code, StatusCode::OK);
        assert_eq!(dm["conversation"]["id"], "TALPHA.DNEW");
        assert_eq!(
            dm["conversation"]["participant_ids"],
            json!(["TALPHA.UADDED"])
        );
    }
    assert_eq!(dm_open_requests.load(Ordering::SeqCst), 1);
    let (code, _, messages) = invoke(
        &app,
        "GET",
        "/slack/conversations/TALPHA.DNEW/messages",
        Some(alpha),
        None,
        None,
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(messages["conversation_id"], "TALPHA.DNEW");

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

fn test_display_account() -> super::store::Account {
    super::store::Account {
        nicknames: Default::default(),
        identity: super::protocol::Identity {
            session_id: Uuid::new_v4(), team_id: "TTEAM".into(),
            user_id: "UME".into(), team_name: "工作区".into(),
        },
        access: String::new(), web_session: true, baseline: 0, imported_token_hash: None, revoked: false,
    }
}

#[test]
fn message_mentions_embed_user_directory_names_and_ids() {
    use super::{mailbox::UserDirectory, protocol::Message, store::Conversation};

    let profile = serde_json::from_value(json!({
        "id":"TTEAM.UYOU","team_id":"TTEAM","name":"林舟","is_bot":false
    })).unwrap();
    let users = UserDirectory::from_disk(Uuid::new_v4(), vec![profile]);
    let message = Message::from_slack(&json!({
        "ts":"100.000001", "text":"交给 <@UYOU> 和 <@WOTHER|替补>，代码 `<@UCODE>`",
        "blocks":[{"type":"section","fields":[
            {"type":"mrkdwn","text":"复核 <@UYOU>"},
            {"type":"plain_text","text":"<@ULITERAL>"}
        ]}]
    }), None).unwrap();
    let account = test_display_account();
    account.nicknames.write().unwrap().insert("TTEAM.UYOU".into(), "小林".into());
    let result = Conversation::new("CWORK", "public_channel", "产品讨论")
        .message_result(&message, &account, Some(&users));
    assert_eq!(result["text"], message.text);
    assert_eq!(result["mentions"], json!([
        {"id":"TTEAM.UYOU","name":"林舟","nickname":"小林"},
        {"id":"TTEAM.WOTHER","name":"替补","nickname":null}
    ]));
}

#[test]
fn conversation_summary_tracks_unread_mentions_and_latest_reply() {
    use super::{
        protocol::{Identity, Message},
        store::{Account, Conversation},
    };

    let account = Account {
        nicknames: Default::default(),
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
    let apis = meta["apis"].as_array().unwrap();
    assert!(apis.iter().all(|api| api["path"] != "/slack/status"));
    assert!(apis.iter().any(|api| {
        api["method"] == "POST"
            && api["path"] == "/slack/teams/{team_id}/reset-credentials"
            && api["access"] == "local"
    }));
}
