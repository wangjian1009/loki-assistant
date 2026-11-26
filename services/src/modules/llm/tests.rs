use std::{convert::Infallible, fs, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{DefaultBodyLimit, OriginalUri},
    http::{HeaderMap, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Semaphore, task::JoinHandle, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

use super::{
    Monitor, config::notification_router, protocol::Protocol, service::LlmService,
    usage::UsageReader,
};
use crate::{
    modules::{
        config::{self, service::ConfigService},
        password::service::{
            CommandFuture, CommandOutput, CommandRunner, PasswordEnvironment, PasswordService,
        },
    },
    runtime::RuntimeState,
};

const CODEX_FIRST: &str = concat!(
    "event: response.created\r\ndata: {\"type\":\"response.created\"}\r\n\r\n",
    "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"正在查看\"}\r\n\r\n",
    "data: {\"type\":\"response.output_item.added\",\"output_index\":1,\"item\":{\"type\":\"function_call\",\"name\":\"read_file\",\"call_id\":\"call-2\",\"arguments\":\"\"}}\r\n\r\n",
    "data: {\"type\":\"response.function_call_arguments.delta\",\"output_index\":1,\"delta\":\"{\\\"path\\\":\\\"README.org\\\"}\"}\r\n\r\n",
);
const CODEX_LAST: &str = "event: response.completed\r\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":11,\"output_tokens\":7}}}\r\n\r\n";
const CLAUDE_STREAM: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":23,\"output_tokens\":1,\"cache_read_input_tokens\":8}}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3}}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

async fn invoke(
    app: &Router,
    method: Method,
    path: &str,
    auth: Option<(&str, &str)>,
    body: Option<Value>,
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some((name, value)) = auth {
        request = request.header(name, value);
    }
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    timeout(
        Duration::from_secs(5),
        app.clone().oneshot(request.body(body).unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn json_body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&bytes)))
}

#[tokio::test]
async fn monitoring_filters_and_independent_settings_survive_restart() {
    let directory = Directory::new();
    let (app, _notifications) = persistent_router(&directory).await;
    let metadata = json_body(invoke(&app, Method::GET, "/meta/llm", None, None).await).await;
    let items = metadata["items"].as_array().unwrap();
    let filter = items
        .iter()
        .find(|item| item["key"] == "monitoring_filter")
        .unwrap();
    let history = items
        .iter()
        .find(|item| item["key"] == "monitoring_history")
        .unwrap();
    assert_eq!(filter["data_type"], "structure");
    assert_eq!(history["data_type"], "list");
    assert_eq!(history["list"]["item_type"], "structure");
    assert_eq!(filter["structure"]["type_name"], "Monitor");
    assert_eq!(filter["structure"], history["structure"]);
    let settings = json_body(invoke(&app, Method::GET, "/llm/monitoring", None, None).await).await;
    assert_eq!(settings, json!({"filter":{},"history":[]}));
    let client_id = Uuid::new_v4();
    for index in 0..12 {
        let filter = json!({
            "protocols":["codex"],
            "clients":[client_id],
            "upstreams":[format!("work-{index}"), format!("work-{index}")],
        });
        let response = invoke(
            &app,
            Method::PATCH,
            "/llm/monitoring",
            None,
            Some(json!({"filter":filter})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    for patch in [
        json!({"enabled":true}),
        json!({"retention":8}),
        json!({"filter":null}),
    ] {
        assert_eq!(
            invoke(&app, Method::PATCH, "/llm/monitoring", None, Some(patch))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    for (key, body, status) in [
        ("monitoring_retention", json!(-1), StatusCode::BAD_REQUEST),
        (
            "monitoring_filter",
            json!({"protocols":["invalid"]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "monitoring_history",
            json!([{"clients":["invalid"]}]),
            StatusCode::BAD_REQUEST,
        ),
        ("monitoring_retention", json!(8), StatusCode::OK),
    ] {
        assert_eq!(
            invoke(
                &app,
                Method::PUT,
                &format!("/editing/llm/{key}"),
                None,
                Some(body)
            )
            .await
            .status(),
            status
        );
    }
    let disabled = json_body(
        invoke(
            &app,
            Method::PATCH,
            "/llm/monitoring",
            None,
            Some(json!({"filter":{}})),
        )
        .await,
    )
    .await;
    assert_eq!(disabled["filter"], json!({}));
    assert_eq!(disabled["history"].as_array().unwrap().len(), 10);
    assert_eq!(disabled["history"][0]["upstreams"], json!(["work-11"]));
    assert_eq!(disabled["history"][9]["upstreams"], json!(["work-2"]));
    let (restarted, _notifications) = persistent_router(&directory).await;
    let settings =
        json_body(invoke(&restarted, Method::GET, "/llm/monitoring", None, None).await).await;
    assert_eq!(settings, disabled);
    let restored = json_body(
        invoke(
            &restarted,
            Method::PATCH,
            "/llm/monitoring",
            None,
            Some(json!({"filter":settings["history"][5]})),
        )
        .await,
    )
    .await;
    assert_eq!(
        restored["filter"],
        json!({"protocols":["codex"],"clients":[client_id],"upstreams":["work-6"]})
    );
    assert_eq!(restored["history"][0], restored["filter"]);
    assert_eq!(restored["history"].as_array().unwrap().len(), 10);
    let values = json_body(invoke(&restarted, Method::GET, "/data/llm", None, None).await).await;
    assert_eq!(values["values"]["monitoring_retention"]["value"], 8);
    let custom_history = json!([{"protocols":["claude"]}]);
    assert_eq!(
        invoke(
            &restarted,
            Method::PUT,
            "/editing/llm/monitoring_history",
            None,
            Some(custom_history.clone())
        )
        .await
        .status(),
        StatusCode::OK
    );
    let edited =
        json_body(invoke(&restarted, Method::GET, "/llm/monitoring", None, None).await).await;
    assert_eq!(edited["filter"], restored["filter"]);
    assert_eq!(edited["history"], custom_history);
}

async fn provider(app: &Router, id: &str, record: Value) {
    let response = invoke(
        app,
        Method::PUT,
        &format!("/editing/llm/providers/{id}"),
        None,
        Some(record),
    )
    .await;
    let status = response.status();
    let body = json_body(response).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn issue(app: &Router, upstream: &str) -> (String, String) {
    let response = invoke(
        app,
        Method::POST,
        "/llm/clients",
        None,
        Some(json!({ "upstream": upstream })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = json_body(response).await;
    (
        body["client"]["id"].as_str().unwrap().into(),
        body["client"]["token"].as_str().unwrap().into(),
    )
}

async fn serve(app: Router) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}"), task)
}

fn sse(body: Body) -> Response {
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

#[tokio::test]
async fn proxies_native_streams_and_accounts_to_the_binding_at_request_start() {
    let gate = Arc::new(Semaphore::new(0));
    let response_gate = gate.clone();
    let (upstream, server) = serve(
        Router::new()
            .route(
                "/codex/v1/responses",
                post(move |headers: HeaderMap, Json(request): Json<Value>| {
                    let gate = response_gate.clone();
                    async move {
                        assert!(!headers.contains_key(header::AUTHORIZATION));
                        assert_eq!(request, native_responses_request());
                        let first = stream::once(async {
                            Ok::<_, Infallible>(Bytes::from_static(CODEX_FIRST.as_bytes()))
                        });
                        let last = stream::once(async move {
                            gate.acquire().await.unwrap().forget();
                            Ok(Bytes::from_static(CODEX_LAST.as_bytes()))
                        });
                        sse(Body::from_stream(first.chain(last)))
                    }
                }),
            )
            .route(
                "/claude/v1/messages",
                post(
                    |headers: HeaderMap,
                     OriginalUri(uri): OriginalUri,
                     Json(request): Json<Value>| async move {
                        assert!(!headers.contains_key("x-api-key"));
                        assert_eq!(uri.query(), Some("beta=true"));
                        assert_eq!(headers["anthropic-version"], "2023-06-01");
                        assert_eq!(request, native_messages_request());
                        sse(Body::from(CLAUDE_STREAM))
                    },
                ),
            )
            .route(
                "/claude/v1/messages/count_tokens",
                post(|Json(request): Json<Value>| async move {
                    assert_eq!(request, json!({}));
                    Json(json!({"input_tokens":99}))
                }),
            )
            .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
    )
    .await;

    let app = crate::http::router(Arc::new(RuntimeState::default()));
    assert_eq!(
        invoke(
            &app,
            Method::PATCH,
            "/llm/monitoring",
            None,
            Some(json!({"filter":{"protocols":["codex","claude"]}}))
        )
        .await
        .status(),
        StatusCode::OK,
    );
    provider(
        &app,
        "a",
        json!({ "codex_uri": format!("{upstream}/codex/v1/") }),
    )
    .await;
    provider(
        &app,
        "b",
        json!({ "claude_uri": format!("{upstream}/claude/v1") }),
    )
    .await;
    let (id, token) = issue(&app, "a").await;
    let bearer = format!("Bearer {token}");
    let response = invoke(
        &app,
        Method::POST,
        "/llm/codex/v1/responses",
        Some(("authorization", &bearer)),
        Some(native_responses_request()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut old_stream = response.into_body().into_data_stream();
    assert_eq!(
        timeout(Duration::from_secs(3), old_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        CODEX_FIRST
    );
    let snapshot = json_body(invoke(&app, Method::GET, "/llm/requests", None, None).await).await;
    let active = snapshot["active"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    let codex_id = active[0]["id"].clone();
    assert_eq!(active[0]["upstream"], "a");
    assert_eq!(active[0]["model"], "codex-model");
    assert_eq!(active[0]["phase"], "responding");
    assert!(active[0]["first_byte_ms"].is_u64());
    assert!(active[0]["usage"].is_null());
    assert_eq!(active[0]["has_detail"], true);
    assert!(active[0].get("instructions").is_none());
    assert!(active[0].get("request_content").is_none());
    assert_eq!(snapshot["recent"], json!([]));
    let codex_path = format!("/llm/requests/{}", codex_id.as_str().unwrap());
    let detail = invoke(&app, Method::GET, &codex_path, None, None).await;
    assert_eq!(detail.headers()[header::CACHE_CONTROL], "no-store");
    let detail = json_body(detail).await;
    assert_eq!(detail["item"]["phase"], "responding");
    assert_eq!(
        detail["item"]["request_content"]["settings"]["tool_choice"],
        "auto"
    );
    assert_eq!(
        detail["item"]["request_content"]["tools"][0]["name"],
        "read_file"
    );
    assert_eq!(
        detail["item"]["request_content"]["blocks"][0]["kind"],
        "tool_result"
    );
    assert_eq!(
        detail["item"]["request_content"]["blocks"][1]["text"]
            .as_str()
            .unwrap()
            .len(),
        5_300_000
    );
    let raw = invoke(
        &app,
        Method::GET,
        &format!("{codex_path}/raw/request"),
        None,
        None,
    )
    .await;
    let raw = to_bytes(raw.into_body(), 8 * 1024 * 1024).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&raw).unwrap(),
        native_responses_request()
    );
    assert_eq!(
        detail["item"]["response_content"],
        json!({
            "complete":false,"truncated":false,
            "blocks":[
                {"kind":"message","role":"assistant","text":"正在查看"},
                {"kind":"tool_call","name":"read_file","id":"call-2","text":"{\"path\":\"README.org\"}"}
            ]
        })
    );
    assert_eq!(
        detail["item"]["instructions"]["blocks"],
        json!([
            {"source":"instructions","role":null,"text":"顶层示例"},
            {"source":"input[0]","role":"system","text":"系统示例"},
            {"source":"input[1].content[0].text","role":"developer","text":"开发者示例"}
        ])
    );
    let response = invoke(
        &app,
        Method::PUT,
        &format!("/llm/clients/{id}"),
        None,
        Some(json!({"upstream":"b"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = invoke(
        &app,
        Method::POST,
        "/llm/codex/v1/responses",
        Some(("authorization", &bearer)),
        Some(json!({})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        json_body(response).await["error"]["code"],
        "llm-protocol-unavailable"
    );

    let response = app
        .clone()
        .oneshot(
            Request::post("/llm/claude/v1/messages?beta=true")
                .header("x-api-key", &token)
                .header("anthropic-version", "2023-06-01")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(native_messages_request().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 1048576).await.unwrap(),
        CLAUDE_STREAM
    );
    let snapshot = json_body(invoke(&app, Method::GET, "/llm/requests", None, None).await).await;
    assert_eq!(snapshot["active"][0]["id"], codex_id);
    assert_eq!(snapshot["active"][0]["upstream"], "a");
    assert_eq!(snapshot["recent"][0]["model"], "claude-model");
    assert_eq!(snapshot["recent"][0]["phase"], "completed");
    assert_eq!(snapshot["recent"][0]["has_detail"], true);
    assert!(snapshot["recent"][0].get("response_content").is_none());
    let claude_path = format!(
        "/llm/requests/{}",
        snapshot["recent"][0]["id"].as_str().unwrap()
    );
    let claude_detail = json_body(invoke(&app, Method::GET, &claude_path, None, None).await).await;
    assert_eq!(
        claude_detail["item"]["instructions"]["blocks"],
        json!([{"source":"system[0].text","role":"system","text":"Claude 示例"}])
    );
    gate.add_permits(1);
    assert_eq!(old_stream.next().await.unwrap().unwrap(), CODEX_LAST);
    assert!(old_stream.next().await.is_none());

    let response = invoke(
        &app,
        Method::POST,
        "/llm/claude/v1/messages/count_tokens",
        Some(("x-api-key", &token)),
        Some(json!({})),
    )
    .await;
    assert_eq!(json_body(response).await["input_tokens"], 99);
    let snapshot = json_body(invoke(&app, Method::GET, "/llm/requests", None, None).await).await;
    assert_eq!(snapshot["active"], json!([]));
    assert_eq!(snapshot["recent"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["recent"][0]["id"], codex_id);
    assert_eq!(snapshot["recent"][0]["phase"], "completed");
    assert_eq!(
        snapshot["recent"][0]["usage"],
        json!({"input_tokens":11,"output_tokens":7,"complete":true})
    );
    let report = json_body(invoke(&app, Method::GET, "/llm/usage", None, None).await).await;
    assert_eq!(
        report["usage"],
        json!([
            {"client_id":id,"upstream":"a","input_tokens":11,"output_tokens":7,"requests":1,"incomplete_requests":0},
            {"client_id":id,"upstream":"b","input_tokens":23,"output_tokens":5,"requests":1,"incomplete_requests":0}
        ])
    );
    let persisted = json_body(invoke(&app, Method::GET, "/llm/requests", None, None).await).await;
    assert_eq!(persisted, snapshot);
    let codex_recent = json_body(invoke(&app, Method::GET, &codex_path, None, None).await).await;
    assert_eq!(
        codex_recent["item"]["instructions"]["blocks"],
        detail["item"]["instructions"]["blocks"]
    );
    assert_eq!(codex_recent["item"]["response_content"]["complete"], true);
    assert_eq!(
        codex_recent["item"]["response_content"]["blocks"],
        detail["item"]["response_content"]["blocks"]
    );
    let history_path = format!("/llm/usage/requests/{}", codex_id.as_str().unwrap());
    let history = json_body(invoke(&app, Method::GET, &history_path, None, None).await).await;
    assert!(history["item"].get("instructions").is_none());
    server.abort();
}

fn native_responses_request() -> Value {
    json!({
        "model":"codex-model",
        "instructions":"顶层示例",
        "input":[
            {"role":"system","content":"系统示例"},
            {"role":"developer","content":[{"type":"input_text","text":"开发者示例"}]},
            {"type":"function_call_output","call_id":"call-1","output":"工具结果示例"},
            {"role":"user","content":"a".repeat(5_300_000)}
        ],
        "tools":[{"type":"function","name":"read_file","parameters":{"type":"object"}}],
        "tool_choice":"auto",
        "stream":true
    })
}

fn native_messages_request() -> Value {
    json!({
        "model":"claude-model",
        "system":[{"type":"text","text":"Claude 示例"}],
        "stream":true
    })
}

#[derive(Debug)]
struct FixtureGpg;

impl CommandRunner for FixtureGpg {
    fn run<'a>(
        &'a self,
        _: &'a str,
        args: &'a [String],
        input: Option<&'a [u8]>,
    ) -> CommandFuture<'a> {
        Box::pin(async move {
            let stdout = if args.iter().any(|arg| arg == "--show-only-session-key") {
                b"[GNUPG:] SESSION_KEY 9:1111\n".to_vec()
            } else if let Some(input) = input {
                input.to_vec()
            } else {
                fs::read(args.last().unwrap()).unwrap()
            };
            Ok(CommandOutput {
                status: Some(0),
                stdout,
            })
        })
    }
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("loki-llm-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn persistent_router(directory: &Directory) -> (Router, Arc<Router>) {
    let mut environment = PasswordEnvironment::new("/bin/echo".into(), directory.0.clone());
    environment.recipients = vec!["fixture".into()];
    let passwords = Arc::new(PasswordService::with_fixture_runner(
        Some(environment),
        Arc::new(FixtureGpg),
        Duration::from_secs(3),
    ));
    let config = ConfigService::load(directory.0.join("config.yml"))
        .unwrap()
        .with_password_service(passwords);
    let config = config::Module::from_service(config);
    let monitor = Monitor::load(directory.0.join("llm-usage.json")).unwrap();
    let llm = Arc::new(LlmService::new(
        config.service(),
        monitor,
        Some(directory.0.join("requests")),
    ));
    let notifications = Arc::new(notification_router(llm.clone()));
    config.service().attach_notifications(&notifications);
    llm.reload_authorization().await.unwrap();
    (config.router().merge(llm.router()), notifications)
}

#[tokio::test]
async fn persists_bindings_and_partial_usage_without_leaking_credentials() {
    let (upstream, server) = serve(
        Router::new()
            .route(
                "/v1/models",
                get(
                    |headers: HeaderMap, OriginalUri(uri): OriginalUri| async move {
                        assert_eq!(
                            headers[header::AUTHORIZATION],
                            "Bearer shared-upstream-token"
                        );
                        assert!(!headers.contains_key("x-api-key"));
                        assert_eq!(uri.query(), Some("client_version=1"));
                        Json(json!({"data":[]}))
                    },
                ),
            )
            .route(
                "/v1/responses/compact",
                post(|headers: HeaderMap| async move {
                    assert_eq!(
                        headers[header::AUTHORIZATION],
                        "Bearer shared-upstream-token"
                    );
                    Json(json!({"usage":{"input_tokens":4}}))
                }),
            )
            .route(
                "/v1/messages",
                post(|headers: HeaderMap| async move {
                    assert_eq!(headers["x-api-key"], "shared-upstream-token");
                    assert!(!headers.contains_key(header::AUTHORIZATION));
                    Json(json!({"usage":{"input_tokens":31,"output_tokens":0}}))
                }),
            ),
    )
    .await;
    let directory = Directory::new();
    let (app, _notifications) = persistent_router(&directory).await;
    let root = format!("{upstream}/v1");
    provider(
        &app,
        "shared",
        json!({
            "codex_uri":root,"claude_uri":root,
            "token":"shared-upstream-token",
        }),
    )
    .await;
    let (id, token) = issue(&app, "shared").await;
    let listed = json_body(invoke(&app, Method::GET, "/llm/clients", None, None).await).await;
    assert_eq!(listed["clients"], json!([{"id":id,"upstream":"shared"}]));
    let stored = fs::read_to_string(directory.0.join("config.yml")).unwrap();
    assert!(!stored.contains("shared-upstream-token"));
    let stored: Value = serde_yaml_ng::from_str(&stored).unwrap();
    assert_eq!(
        stored["modules"]["llm"]["providers"][0]["token"]["accepted"]["value"],
        "loki-assistant/llm/shared.gpg"
    );
    assert!(directory.0.join("loki-assistant/llm/shared.gpg").is_file());
    let updated = invoke(
        &app,
        Method::PATCH,
        "/editing/llm/providers/shared",
        None,
        Some(json!({"codex_uri":"file:///tmp"})),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bearer = format!("Bearer {token}");
    let response = invoke(
        &app,
        Method::GET,
        "/llm/codex/v1/models?client_version=1",
        Some(("authorization", &bearer)),
        None,
    )
    .await;
    assert_eq!(json_body(response).await, json!({"data":[]}));
    let response = invoke(
        &app,
        Method::POST,
        "/llm/codex/v1/responses/compact",
        Some(("authorization", &bearer)),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        json_body(response).await["usage"],
        json!({"input_tokens":4})
    );
    let recent =
        json_body(invoke(&app, Method::GET, "/llm/usage/requests?limit=1", None, None).await).await;
    assert_eq!(recent["items"][0]["usage"]["input_reported_tokens"], 4);
    assert_eq!(recent["items"][0]["usage"]["output_tokens"], Value::Null);
    assert_eq!(recent["items"][0]["usage"]["complete"], false);

    let _ = json_body(invoke(&app, Method::GET, "/llm/usage", None, None).await).await;
    let (restarted, _notifications) = persistent_router(&directory).await;
    let response = invoke(
        &restarted,
        Method::POST,
        "/llm/claude/v1/messages",
        Some(("authorization", &bearer)),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        json_body(response).await["usage"],
        json!({"input_tokens":31,"output_tokens":0})
    );
    let report = json_body(invoke(&restarted, Method::GET, "/llm/usage", None, None).await).await;
    assert_eq!(report["storage_error"], false);
    assert_eq!(
        report["usage"],
        json!([
            {"client_id":id,"upstream":"shared","input_tokens":35,"output_tokens":0,"requests":2,"incomplete_requests":1}
        ])
    );
    let response = invoke(
        &restarted,
        Method::DELETE,
        &format!("/llm/clients/{id}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = invoke(
        &restarted,
        Method::GET,
        "/llm/codex/v1/models",
        Some(("authorization", &bearer)),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    server.abort();
}

#[test]
fn parses_split_frames_and_keeps_usage_in_an_interrupted_stream() {
    let mut usage = UsageReader::new(Protocol::Claude, true);
    let until_stop = CLAUDE_STREAM.split("event: message_stop").next().unwrap();
    for byte in until_stop.as_bytes().chunks(1) {
        usage.push(byte);
    }
    let (observed, failed) = usage.finish(false);
    let usage = observed.usage;
    assert_eq!(usage.input_tokens, Some(23));
    assert_eq!(usage.output_tokens, Some(5));
    assert!(!usage.complete);
    assert!(failed);

    let mut usage = UsageReader::new(Protocol::Codex, true);
    for chunk in CODEX_LAST.as_bytes().chunks(7) {
        usage.push(chunk);
    }
    let (observed, _) = usage.finish(true);
    let usage = observed.usage;
    assert!(usage.complete);
    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
}

#[tokio::test]
async fn dropping_an_active_stream_preserves_partial_usage() {
    let first = CLAUDE_STREAM.split("event: message_delta").next().unwrap();
    let (upstream, server) = serve(Router::new().route(
        "/v1/messages",
        post(move || async move {
            let start =
                stream::once(
                    async move { Ok::<_, Infallible>(Bytes::from_static(first.as_bytes())) },
                );
            sse(Body::from_stream(start.chain(stream::pending())))
        }),
    ))
    .await;
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    provider(
        &app,
        "claude",
        json!({"claude_uri": format!("{upstream}/v1")}),
    )
    .await;
    let (id, token) = issue(&app, "claude").await;
    let response = invoke(
        &app,
        Method::POST,
        "/llm/claude/v1/messages",
        Some(("x-api-key", &token)),
        Some(json!({"model":"requested-model","stream":true})),
    )
    .await;
    let mut body = response.into_body().into_data_stream();
    assert_eq!(
        timeout(Duration::from_secs(3), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        first
    );
    drop(body);

    let recent = timeout(Duration::from_secs(3), async {
        loop {
            let requests = json_body(
                invoke(&app, Method::GET, "/llm/usage/requests?limit=1", None, None).await,
            )
            .await;
            if requests["items"][0]["outcome"] == "cancelled" {
                break requests;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(recent["items"][0]["model"], "requested-model");
    assert_eq!(recent["items"][0]["usage"]["input_reported_tokens"], 23);
    assert_eq!(recent["items"][0]["usage"]["output_tokens"], 1);
    assert_eq!(recent["items"][0]["usage"]["complete"], false);
    let report = json_body(invoke(&app, Method::GET, "/llm/usage", None, None).await).await;
    assert_eq!(
        report["usage"],
        json!([
            {"client_id":id,"upstream":"claude","input_tokens":23,"output_tokens":1,"requests":1,"incomplete_requests":1}
        ])
    );
    server.abort();
}

#[tokio::test]
async fn forwards_an_arbitrary_http_request_without_buffering_or_rewriting_its_body() {
    const PAYLOAD: &[u8] = b"first\x00second\xff";
    let first_chunk = Arc::new(Semaphore::new(0));
    let upstream_first_chunk = first_chunk.clone();
    let (upstream, server) = serve(Router::new().fallback(
        move |request: axum::extract::Request| {
            let first_chunk = upstream_first_chunk.clone();
            async move {
                assert_eq!(request.method(), Method::PATCH);
                assert_eq!(
                    request.uri().path_and_query().unwrap().as_str(),
                    "/vendor/v1/files/name%2Fid?key=a%2Bb&key=c"
                );
                assert!(!request.headers().contains_key(header::AUTHORIZATION));
                assert_eq!(
                    request.headers()[header::CONTENT_TYPE],
                    "application/octet-stream"
                );
                assert_eq!(request.headers()[header::ACCEPT_ENCODING], "gzip");
                assert_eq!(request.headers()["x-custom"], "keep");
                assert_eq!(request.headers()[header::COOKIE], "hint=1");
                let mut bytes = Vec::new();
                let mut body = request.into_body().into_data_stream();
                bytes.extend_from_slice(&body.next().await.unwrap().unwrap());
                first_chunk.add_permits(1);
                while let Some(chunk) = body.next().await {
                    bytes.extend_from_slice(&chunk.unwrap());
                }
                assert_eq!(bytes, PAYLOAD);
                (StatusCode::IM_A_TEAPOT, [("x-origin", "upstream")], bytes)
            }
        },
    ))
    .await;
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    provider(
        &app,
        "raw",
        json!({"codex_uri": format!("{upstream}/vendor/v1/")}),
    )
    .await;
    let (_, token) = issue(&app, "raw").await;
    let first = stream::once(async { Ok::<_, Infallible>(Bytes::from_static(&PAYLOAD[..6])) });
    let last = stream::once(async move {
        first_chunk.acquire().await.unwrap().forget();
        Ok(Bytes::from_static(&PAYLOAD[6..]))
    });
    let request = Request::builder()
        .method(Method::PATCH)
        .uri("/llm/codex/v1/files/name%2Fid?key=a%2Bb&key=c")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_ENCODING, "gzip")
        .header(header::COOKIE, "hint=1")
        .header("x-custom", "keep")
        .body(Body::from_stream(first.chain(last)))
        .unwrap();
    let response = timeout(Duration::from_secs(5), app.oneshot(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(response.headers()["x-origin"], "upstream");
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), PAYLOAD);
    server.abort();
}

#[tokio::test]
async fn groups_sessions_and_restores_them_alongside_legacy_totals() {
    let (upstream, server) = serve(Router::new().fallback(post(
        |headers: HeaderMap, OriginalUri(uri): OriginalUri| async move {
            if uri.path() == "/v1/messages" {
                assert_eq!(headers["x-loki-session-id"], "shared-session");
            } else {
                assert_eq!(uri.path(), "/v1/responses");
            }
            Json(json!({"usage":{"input_tokens":3,"output_tokens":2}}))
        },
    )))
    .await;
    let directory = Directory::new();
    let path = directory.0.join("llm-usage.json");
    let legacy_id = Uuid::nil().to_string();
    fs::write(
        &path,
        json!({ legacy_id.clone(): {
            "legacy": {"input_tokens":8,"output_tokens":9,"requests":1,"incomplete_requests":0}
        }})
        .to_string(),
    )
    .unwrap();
    let (app, _notifications) = persistent_router(&directory).await;
    provider(&app, "a", json!({"codex_uri":format!("{upstream}/v1")})).await;
    provider(
        &app,
        "b",
        json!({"codex_uri":format!("{upstream}/v1"),"claude_uri":format!("{upstream}/v1")}),
    )
    .await;
    let (client_id, token) = issue(&app, "a").await;
    let bearer = format!("Bearer {token}");

    for (route, headers, expected) in [
        (
            "/llm/codex/v1/responses",
            &[
                ("thread-id", "shared-session"),
                ("session-id", "codex-session"),
                ("session_id", "dsh-session"),
            ][..],
            Some("shared-session"),
        ),
        (
            "/llm/codex/v1/responses",
            &[("session_id", "dsh-session")],
            Some("dsh-session"),
        ),
        (
            "/llm/codex/v1/responses",
            &[("session-id", "invalid id"), ("session_id", "dsh-session")],
            Some("dsh-session"),
        ),
        (
            "/llm/codex/v1/responses",
            &[
                ("x-loki-session-id", "ignored"),
                ("session-id", "shared-session"),
                ("session_id", "dsh-session"),
            ],
            Some("shared-session"),
        ),
        (
            "/llm/codex/v1/responses",
            &[("x-loki-session-id", "shared-session")],
            None,
        ),
    ] {
        let mut request = Request::post(route).header(header::AUTHORIZATION, &bearer);
        for &(name, value) in headers {
            request = request.header(name, value);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            json_body(response).await["usage"],
            json!({"input_tokens":3,"output_tokens":2})
        );
        let report =
            json_body(invoke(&app, Method::GET, "/llm/usage/requests?limit=1", None, None).await)
                .await;
        assert_eq!(report["items"][0]["session_id"], json!(expected));
    }

    let response = invoke(
        &app,
        Method::PUT,
        &format!("/llm/clients/{client_id}"),
        None,
        Some(json!({"upstream":"b"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let request = Request::post("/llm/codex/v1/responses")
        .header(header::AUTHORIZATION, &bearer)
        .header("thread-id", "shared-session")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        json_body(response).await["usage"],
        json!({"input_tokens":3,"output_tokens":2})
    );
    let request = Request::post("/llm/claude/v1/messages")
        .header("x-api-key", token)
        .header("x-loki-session-id", "shared-session")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        json_body(response).await["usage"],
        json!({"input_tokens":3,"output_tokens":2})
    );
    let report =
        json_body(invoke(&app, Method::GET, "/llm/usage/requests?limit=1", None, None).await).await;
    assert_eq!(report["items"][0]["session_id"], Value::Null);

    let _ = json_body(invoke(&app, Method::GET, "/llm/usage", None, None).await).await;
    let (restarted, _notifications) = persistent_router(&directory).await;
    let sessions =
        json_body(invoke(&restarted, Method::GET, "/llm/usage/sessions", None, None).await).await;
    assert_eq!(
        sessions["usage"],
        json!([
            {"client_id":client_id,"upstream":"a","session_id":"dsh-session","input_tokens":6,"output_tokens":4,"requests":2,"incomplete_requests":0},
            {"client_id":client_id,"upstream":"a","session_id":"shared-session","input_tokens":6,"output_tokens":4,"requests":2,"incomplete_requests":0},
            {"client_id":client_id,"upstream":"b","session_id":"shared-session","input_tokens":3,"output_tokens":2,"requests":1,"incomplete_requests":0}
        ])
    );
    let totals = json_body(invoke(&restarted, Method::GET, "/llm/usage", None, None).await).await;
    assert_eq!(
        totals["usage"],
        json!([
            {"client_id":legacy_id,"upstream":"legacy","input_tokens":8,"output_tokens":9,"requests":1,"incomplete_requests":0},
            {"client_id":client_id,"upstream":"a","input_tokens":15,"output_tokens":10,"requests":5,"incomplete_requests":0},
            {"client_id":client_id,"upstream":"b","input_tokens":6,"output_tokens":4,"requests":2,"incomplete_requests":0}
        ])
    );
    server.abort();
}

#[tokio::test]
async fn queries_persisted_usage_by_model_session_and_stable_page() {
    let (upstream, server) = serve(Router::new().route(
        "/v1/responses",
        post(|| async {
            Json(json!({
                "model": "test-model",
                "usage": {
                    "input_tokens": 120,
                    "input_tokens_details":{"cached_tokens":80},
                    "output_tokens":10,
                }
            }))
        }),
    ))
    .await;
    let directory = Directory::new();
    let (app, _notifications) = persistent_router(&directory).await;
    provider(&app, "work", json!({"codex_uri":format!("{upstream}/v1")})).await;
    let created = invoke(
        &app,
        Method::POST,
        "/llm/clients",
        None,
        Some(json!({"name":"editor","upstream":"work"})),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created = json_body(created).await;
    let client = created["client"]["id"].as_str().unwrap();
    let token = created["client"]["token"].as_str().unwrap();
    for _ in 0..2 {
        let request = Request::post("/llm/codex/v1/responses")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("thread-id", "thread-17")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(json_body(response).await["model"], "test-model");
    }
    let path = format!("/llm/usage/requests?client_id={client}&session_id=thread-17&limit=1");
    let response = invoke(&app, Method::GET, &path, None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let data = json_body(response).await;
    let row = &data["items"][0];
    assert_eq!(row["client_name"], "editor");
    assert_eq!(data["client_names"][client], "editor");
    assert_eq!(row["model"], "test-model");
    assert_eq!(row["usage"]["input_uncached_tokens"], 40);
    assert_eq!(row["usage"]["cache_read_tokens"], 80);
    let cursor = data["next_cursor"].as_str().unwrap();
    let next = json_body(
        invoke(
            &app,
            Method::GET,
            &format!("{path}&cursor={cursor}"),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_ne!(next["items"][0]["id"], row["id"]);
    assert_eq!(next["next_cursor"], Value::Null);
    let response = invoke(
        &app,
        Method::GET,
        &format!("/llm/usage/requests?limit=1&cursor={cursor}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let detail = json_body(
        invoke(
            &app,
            Method::GET,
            &format!("/llm/usage/requests/{}", row["id"].as_str().unwrap()),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        detail["raw_usage"]["input_tokens_details"]["cached_tokens"],
        80
    );
    assert_eq!(detail["item"]["client_name"], "editor");

    let summary = json_body(
        invoke(
            &app,
            Method::GET,
            "/llm/usage/summary?group_by=model&bucket=month",
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(summary["items"][0]["group"], "test-model");
    assert_eq!(summary["totals"]["requests"], 2);
    assert_eq!(summary["totals"]["cache_read_tokens"], 160);
    let options = json_body(
        invoke(
            &app,
            Method::GET,
            "/llm/usage/options?dimension=model&q=test",
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(options["items"], json!([{"value":"test-model"}]));
    let sessions = json_body(
        invoke(
            &app,
            Method::GET,
            &format!("/llm/usage/sessions?client_id={client}&session_id=thread-17&limit=1"),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(sessions["usage"][0]["requests"], 2);
    assert_eq!(sessions["next_cursor"], Value::Null);
    assert_eq!(sessions["client_names"][client], "editor");
    for path in [
        "/llm/usage",
        "/llm/requests",
        "/llm/usage/summary?group_by=client_id",
        "/llm/usage/options?dimension=client_id",
    ] {
        let report = json_body(invoke(&app, Method::GET, path, None, None).await).await;
        assert_eq!(report["client_names"][client], "editor", "{path}");
    }

    let (restarted, _notifications) = persistent_router(&directory).await;
    let report =
        json_body(invoke(&restarted, Method::GET, "/llm/usage/summary", None, None).await).await;
    assert_eq!(report["totals"]["requests"], 2);
    assert_eq!(report["totals"]["output_tokens"], 20);
    let response = invoke(
        &restarted,
        Method::DELETE,
        &format!("/llm/clients/{client}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let history = json_body(invoke(&restarted, Method::GET, &path, None, None).await).await;
    assert_eq!(history["items"][0]["client_id"], client);
    assert_eq!(history["items"][0]["client_name"], Value::Null);
    server.abort();
}

#[tokio::test]
async fn configured_client_issues_read_only_token_and_refreshes_its_route_after_confirmation() {
    use axum::routing::get;

    let (upstream, server) = serve(
        Router::new()
            .route("/a/models", get(|| async { "A" }))
            .route("/b/models", get(|| async { "B" })),
    )
    .await;
    let directory = Directory::new();
    let (app, _notifications) = persistent_router(&directory).await;
    provider(&app, "a", json!({"codex_uri":format!("{upstream}/a")})).await;
    provider(&app, "b", json!({"codex_uri":format!("{upstream}/b")})).await;

    let response = invoke(
        &app,
        Method::POST,
        "/editing/llm/clients",
        None,
        Some(json!({"name":"editor","upstream":"a"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let created = json_body(response).await;
    assert_eq!(created["status"], "confirmed");
    assert_eq!(created["value"]["name"], "editor");
    let id = created["value"]["id"].as_str().unwrap();
    let path = &format!("/editing/llm/clients/{id}");
    let token = created["value"]["token"].as_str().unwrap();
    assert!(token.starts_with("loki-llm-"));
    let bearer = format!("Bearer {token}");
    let request = |app: &Router| {
        let app = app.clone();
        let bearer = bearer.clone();
        async move {
            let response = invoke(
                &app,
                Method::GET,
                "/llm/codex/v1/models",
                Some(("authorization", &bearer)),
                None,
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            to_bytes(response.into_body(), 4096).await.unwrap()
        }
    };
    assert_eq!(request(&app).await, "A");

    let response = invoke(
        &app,
        Method::PATCH,
        path,
        None,
        Some(json!({"token":"fake"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = invoke(
        &app,
        Method::PATCH,
        path,
        None,
        Some(json!({"upstream":"b","name":"renamed"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["value"]["token"], token);
    let candidates = json_body(
        invoke(
            &app,
            Method::GET,
            "/config/options/llm/client_id",
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        candidates,
        json!({"options":[{"key":id,"label":"renamed"}]})
    );
    let metadata = json_body(invoke(&app, Method::GET, "/meta/llm", None, None).await).await;
    assert_eq!(
        metadata["types"]["protocol"]["enum"],
        json!(["codex", "claude"])
    );
    assert_eq!(
        metadata["types"]["provider_id"]["reference"]["label_field"],
        "id"
    );
    let filter = metadata["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == "monitoring_filter")
        .unwrap();
    assert_eq!(
        filter["structure"]["fields"][0]["list"]["item_type"],
        json!({"ref":"llm.protocol"})
    );
    assert_eq!(request(&app).await, "B");
    let (restarted, _notifications) = persistent_router(&directory).await;
    assert_eq!(request(&restarted).await, "B");
    let records =
        json_body(invoke(&restarted, Method::GET, "/data/llm/clients", None, None).await).await;
    assert_eq!(records["value"][0]["token"], token);
    assert_eq!(records["value"][0]["name"], "renamed");
    assert_eq!(records["value"][0]["id"], id);

    let response = invoke(&restarted, Method::DELETE, path, None, None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = invoke(
        &restarted,
        Method::GET,
        "/llm/codex/v1/models",
        Some(("authorization", &bearer)),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    server.abort();
}

#[tokio::test]
async fn chat_completions_records_the_model_with_or_without_upstream_metadata() {
    let (upstream, server) = serve(Router::new().route(
        "/v1/chat/completions",
        post(|Json(request): Json<Value>| async move {
            assert_eq!(request["model"], "requested-model");
            if request["stream"] == true {
                sse(Body::from(concat!(
                    "data: {\"object\":\"chat.completion.chunk\",\"model\":\"served-model\",\"choices\":[]}\n\n",
                    "data: {\"object\":\"chat.completion.chunk\",\"model\":\"served-model\",\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
                    "data: [DONE]\n\n"
                )))
            } else {
                (StatusCode::BAD_REQUEST, Json(json!({"error":{"message":"rejected"}}))).into_response()
            }
        }),
    ))
    .await;
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    assert_eq!(
        invoke(
            &app,
            Method::PATCH,
            "/llm/monitoring",
            None,
            Some(json!({"filter":{"protocols":["codex","claude"]}}))
        )
        .await
        .status(),
        StatusCode::OK,
    );
    provider(&app, "work", json!({"codex_uri":format!("{upstream}/v1")})).await;
    let (_, token) = issue(&app, "work").await;
    let bearer = format!("Bearer {token}");
    for (stream, status, model) in [
        (true, StatusCode::OK, "served-model"),
        (false, StatusCode::BAD_REQUEST, "requested-model"),
    ] {
        let response = invoke(
            &app,
            Method::POST,
            "/llm/codex/v1/chat/completions",
            Some(("authorization", &bearer)),
            Some(json!({
                "model":"requested-model",
                "messages":[
                    {"role":"system","content":[{"type":"text","text":"聊天系统示例"}]},
                    {"role":"user","content":"用户消息"}
                ],
                "stream":stream
            })),
        )
        .await;
        assert_eq!(response.status(), status);
        let _ = to_bytes(response.into_body(), 4096).await.unwrap();
        let history = json_body(
            invoke(
                &app,
                Method::GET,
                &format!("/llm/usage/requests?model={model}"),
                None,
                None,
            )
            .await,
        )
        .await;
        let item = &history["items"][0];
        assert_eq!(item["model"], model);
        let path = format!("/llm/requests/{}", item["id"].as_str().unwrap());
        let detail = json_body(invoke(&app, Method::GET, &path, None, None).await).await;
        assert_eq!(
            detail["item"]["instructions"]["blocks"],
            json!([{
                "source":"messages[0].content[0].text",
                "role":"system",
                "text":"聊天系统示例"
            }])
        );
        if stream {
            assert_eq!(item["outcome"], "completed");
            assert_eq!(item["usage"]["input_uncached_tokens"], 8);
            assert_eq!(item["usage"]["output_tokens"], 3);
            assert_eq!(item["usage"]["complete"], true);
        }
    }
    server.abort();
}
