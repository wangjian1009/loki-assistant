use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::DocService;
use crate::{
    modules::{
        config::service::ConfigService,
        emacs::{
            config::EmacsConfig,
            service::{CommandOutput, CommandRunner, EmacsService, ServiceFuture},
        },
        notification::NotificationService,
        status::StatusService,
    },
    runtime::RuntimeState,
};

#[derive(Debug)]
struct Runner;

impl CommandRunner for Runner {
    fn run<'a>(&'a self, _: &'a str, args: &'a [String]) -> ServiceFuture<'a, CommandOutput> {
        Box::pin(async move {
            assert_eq!(args[0], "--alternate-editor=/usr/bin/true");
            let script = &args[3];
            assert!(script.contains("loki-assistant-doc-request"));
            let path = regex::Regex::new(r"base64-decode-string[^A-Za-z0-9+/=]+([A-Za-z0-9+/=]+)")
                .unwrap()
                .captures(script)
                .unwrap();
            let path =
                PathBuf::from(String::from_utf8(STANDARD.decode(&path[1]).unwrap()).unwrap());
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            let request: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            let document = json!({
                "id": "article", "title": "中文标题", "kind": "article", "domain": "general",
                "node_type": "file", "tags": [], "date": null
            });
            let reply = match request["operation"].as_str() {
                Some("get") => {
                    assert_eq!(request["id"], "article");
                    let mut document = document;
                    document["content"] = json!("中文\n\"引号\" \\ 路径");
                    json!({"result":document})
                }
                Some("capture") if request["target"] == "article" => {
                    assert_eq!(
                        request["info"]["body"].as_str().unwrap().len(),
                        2 * 1024 * 1024
                    );
                    assert!(script.len() < 8192);
                    let mut result = json!({
                        "status":"saved", "url": request["info"]["url"], "document":document
                    });
                    let open = request["info"]["url"] == "https://example.com/open";
                    assert_eq!(request["info"]["open_after_capture"], open);
                    if open {
                        result["opened"] = json!(false);
                    }
                    json!({"result":result})
                }
                Some("capture") => {
                    assert_eq!(request["target"], "unregistered");
                    json!({"error":"capture-target-not-found"})
                }
                Some("capture_read_later") => {
                    assert_eq!(
                        request["info"],
                        json!({
                            "title": "稍后阅读", "url": "https://example.org/links"
                        })
                    );
                    json!({"result":{"entry": read_later_entry()}})
                }
                Some("list_read_later") => {
                    json!({"result":{"entries":[read_later_entry(), read_later_without_url()]}})
                }
                Some("mark_read_later_read") if request["id"] == "日记/网页" => {
                    json!({"result":{"id":request["id"],"read_at":"2026-11-24T01:00:00Z"}})
                }
                Some("mark_read_later_read") => json!({"error":"read-later-not-found"}),
                Some("delete_read_later") if request["id"] == "日记/网页" => {
                    json!({"result":{"id":request["id"]}})
                }
                Some("delete_read_later") => json!({"error":"read-later-not-found"}),
                _ => panic!("未识别文档操作"),
            };
            let result = serde_json::to_string(&STANDARD.encode(reply.to_string())).unwrap();
            let envelope = serde_json::to_string(&json!({"result": result}).to_string()).unwrap();
            Ok(CommandOutput {
                success: true,
                status: Some(0),
                stdout: envelope,
                stderr: String::new(),
            })
        })
    }
}

fn read_later_entry() -> Value {
    json!({
        "id": "日记/网页", "title": "稍后阅读", "url": "https://example.org/links",
        "captured_at": "2026-11-24T00:00:00Z"
    })
}

fn read_later_without_url() -> Value {
    json!({"id":"日记","title":"无链接的标题","url":null,"captured_at":null})
}

async fn request(
    router: Router,
    method: &str,
    path: &str,
    body: Body,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    (
        parts.status,
        parts.headers,
        serde_json::from_slice(&bytes).unwrap(),
    )
}

fn doc_router() -> Router {
    let mut metadata = EmacsConfig::metadata();
    metadata.items[0].default = Some(json!("/fake/emacsclient"));
    let gateway = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(metadata))),
        Arc::new(NotificationService::new()),
        Arc::new(StatusService::new()),
        Arc::new(Runner),
        Duration::from_secs(2),
    ));
    DocService::new(gateway).router()
}

#[tokio::test]
async fn get_and_post_share_the_path_and_forward_opaque_targets_to_emacs() {
    let router = doc_router();
    let page = json!({
        "title": "中文标题", "url": "https://example.com/a",
        "captured_at": "2026-11-18T12:30:00Z", "body": "x".repeat(2 * 1024 * 1024)
    });
    let (status, headers, body) = request(
        router.clone(),
        "POST",
        "/doc/article",
        Body::from(page.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers[header::LOCATION], "/doc/article");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(body["document"]["id"], "article");
    assert!(body.get("opened").is_none());

    let mut open_page = page.clone();
    open_page["url"] = json!("https://example.com/open");
    open_page["open_after_capture"] = json!(true);
    let (status, headers, body) = request(
        router.clone(),
        "POST",
        "/doc/article",
        Body::from(open_page.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers[header::LOCATION], "/doc/article");
    assert_eq!(body["opened"], false);

    let (status, _, body) = request(router.clone(), "GET", "/doc/article", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["document"]["content"], "中文\n\"引号\" \\ 路径");

    let (status, _, body) = request(
        router,
        "POST",
        "/doc/unregistered",
        Body::from(page.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "capture-target-not-found");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("不支持")
    );
}

#[tokio::test]
async fn read_later_routes_forward_requests_and_replies_to_emacs() {
    let router = doc_router();
    let path = "/doc/read-later/entries";
    let (status, headers, body) = request(
        router.clone(),
        "POST",
        path,
        Body::from(json!({"title":"稍后阅读","url":"https://example.org/links"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(body, json!({"api_version":"1","entry":read_later_entry()}));

    let (status, _, body) = request(router.clone(), "GET", path, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"api_version":"1","entries":[read_later_entry(), read_later_without_url()]})
    );

    let (status, _, body) = request(
        router.clone(),
        "POST",
        "/doc/read-later/entries/%E6%97%A5%E8%AE%B0%2F%E7%BD%91%E9%A1%B5/read",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "日记/网页");
    assert_eq!(body["read_at"], "2026-11-24T01:00:00Z");

    let (status, headers, body) = request(
        router.clone(),
        "DELETE",
        "/doc/read-later/entries/%E6%97%A5%E8%AE%B0%2F%E7%BD%91%E9%A1%B5",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(body, json!({"api_version":"1","id":"日记/网页"}));

    let (status, _, body) = request(
        router.clone(),
        "POST",
        "/doc/read-later/entries/missing/read",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "read-later-not-found");

    let (status, _, body) = request(
        router,
        "DELETE",
        "/doc/read-later/entries/missing",
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "read-later-not-found");
}

#[tokio::test]
async fn discovery_exposes_only_the_read_later_list_to_paired_devices() {
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    let (status, _, body) = request(app, "GET", "/discover/modules/doc", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let apis = body["apis"].as_array().unwrap();
    assert_eq!(apis.len(), 6);
    let remote_apis: Vec<_> = apis.iter().filter(|api| api["access"] != "local").collect();
    assert_eq!(remote_apis.len(), 1);
    assert_eq!(remote_apis[0]["access"], "paired");
    assert_eq!(remote_apis[0]["method"], "GET");
    assert_eq!(remote_apis[0]["path"], "/doc/read-later/entries");
    assert!(
        apis.iter().any(|api| {
            api["path"] == "/doc/read-later/entries/{id}" && api["method"] == "DELETE"
        })
    );
    for method in ["GET", "POST"] {
        assert!(apis.iter().any(|api| {
            api["path"] == "/doc/{id}" && api["method"] == method && api["access"] == "local"
        }));
    }
}
