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
                    json!({"result":{
                        "status":"saved", "url": request["info"]["url"], "document":document
                    }})
                }
                Some("capture") => {
                    assert_eq!(request["target"], "unregistered");
                    json!({"error":"capture-target-not-found"})
                }
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

#[tokio::test]
async fn get_and_post_share_the_path_and_forward_opaque_targets_to_emacs() {
    let mut metadata = EmacsConfig::metadata();
    metadata.items[0].default = Some(json!("/fake/emacsclient"));
    let gateway = Arc::new(EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(metadata))),
        Arc::new(NotificationService::new()),
        Arc::new(StatusService::new()),
        Arc::new(Runner),
        Duration::from_secs(2),
    ));
    let router = DocService::new(gateway).router();
    let page = json!({
        "title": "中文标题", "url": "https://example.com/a",
        "captured_at": "2026-11-18T12:30:00Z", "body": "x".repeat(2 * 1024 * 1024)
    })
    .to_string();
    let (status, headers, body) = request(
        router.clone(),
        "POST",
        "/doc/article",
        Body::from(page.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(headers[header::LOCATION], "/doc/article");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(body["document"]["id"], "article");

    let (status, _, body) = request(router.clone(), "GET", "/doc/article", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["document"]["content"], "中文\n\"引号\" \\ 路径");

    let (status, _, body) = request(router, "POST", "/doc/unregistered", Body::from(page)).await;
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
async fn discovery_mounts_the_doc_module_with_local_access() {
    let app = crate::http::router(Arc::new(RuntimeState::default()));
    let (status, _, body) = request(app, "GET", "/discover/modules/doc", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    let apis = body["apis"].as_array().unwrap();
    assert_eq!(apis.len(), 2);
    for method in ["GET", "POST"] {
        assert!(apis.iter().any(|api| {
            api["path"] == "/doc/{id}" && api["method"] == method && api["access"] == "local"
        }));
    }
}
