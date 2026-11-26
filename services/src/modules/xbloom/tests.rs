use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::XbloomService;
use crate::modules::{
    config::service::ConfigService,
    emacs::{
        config::EmacsConfig,
        service::{CommandOutput, CommandRunner, EmacsService, ServiceFuture},
    },
    notification::NotificationService,
    status::StatusService,
};

#[derive(Debug)]
struct Runner;

impl CommandRunner for Runner {
    fn run<'a>(&'a self, _: &'a str, args: &'a [String]) -> ServiceFuture<'a, CommandOutput> {
        Box::pin(async move {
            let script = &args[3];
            assert!(script.contains("loki-assistant-xbloom-service-request"));
            let path = regex::Regex::new(r"base64-decode-string[^A-Za-z0-9+/=]+([A-Za-z0-9+/=]+)")
                .unwrap();
            let argument = &path.captures(script).unwrap()[1];
            let path =
                PathBuf::from(String::from_utf8(STANDARD.decode(argument).unwrap()).unwrap());
            let request: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            let recipe = json!({
                "id": "日记配方", "storage": "daily",
                "data": {
                    "title": "中文咖啡",
                    "brewing": {"dose_g": 15.5, "grinder_enabled": false},
                    "pours": [{"volume_ml": 51.0, "pause_s": 0.0}]
                }
            });
            let result = match request["operation"].as_str().unwrap() {
                "list" => {
                    assert_eq!(request, json!({"operation":"list","q":"咖啡"}));
                    let library_recipe = json!({
                        "id": "库配方", "storage": "library", "data": {"title": "官方咖啡"}
                    });
                    json!({"recipes":[recipe, library_recipe]})
                }
                "create" => {
                    assert_eq!(request["info"]["based_on_id"], "库配方");
                    assert_eq!(
                        request["info"]["recipe"]["brewing"]["grinder_enabled"],
                        false
                    );
                    json!({"recipe":recipe,"indexed":true})
                }
                "get" => {
                    assert_eq!(request["id"], "日记配方");
                    recipe
                }
                "update" => {
                    assert_eq!(request["id"], "日记配方");
                    assert_eq!(request["recipe"]["pours"][0]["pause_s"], 0.0);
                    json!({"recipe":recipe,"indexed":true})
                }
                op => panic!("意外的 Emacs 操作 {op}"),
            };
            let encoded = STANDARD.encode(serde_json::to_vec(&json!({"result":result})).unwrap());
            let result = serde_json::to_string(&encoded).unwrap();
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

#[tokio::test]
async fn recipe_routes_forward_structured_data_to_emacs() {
    let mut metadata = EmacsConfig::metadata();
    metadata.items[0].default = Some(json!("/fake/emacsclient"));
    let gateway = EmacsService::with_runner(
        EmacsConfig::new(Arc::new(ConfigService::new(metadata))),
        Arc::new(NotificationService::new()),
        Arc::new(StatusService::new()),
        Arc::new(Runner),
        Duration::from_secs(2),
    );
    let router = XbloomService::new(Arc::new(gateway)).router();
    let id = "%E6%97%A5%E8%AE%B0%E9%85%8D%E6%96%B9";
    let detail = format!("/xbloom/recipes/{id}");
    let data = json!({
        "title": "中文咖啡",
        "brewing": {"dose_g":15.5,"grinder_enabled":false},
        "pours":[{"volume_ml":51.0,"pause_s":0.0}]
    });

    for (method, uri, body, status) in [
        (
            "GET",
            "/xbloom/recipes?q=%E5%92%96%E5%95%A1",
            Value::Null,
            StatusCode::OK,
        ),
        ("GET", &detail, Value::Null, StatusCode::OK),
        (
            "POST",
            "/xbloom/recipes",
            json!({"recipe":data,"based_on_id":"库配方"}),
            StatusCode::CREATED,
        ),
        ("PUT", &detail, json!({"recipe":data}), StatusCode::OK),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["api_version"], "1");
        let recipe = if uri.contains('?') {
            assert_eq!(response["recipes"].as_array().unwrap().len(), 2);
            assert_eq!(response["recipes"][1]["id"], "库配方");
            assert!(response.get("next_offset").is_none());
            &response["recipes"][0]
        } else {
            &response["recipe"]
        };
        assert_eq!(recipe["id"], "日记配方");
        assert_eq!(recipe["data"]["brewing"]["dose_g"], 15.5);
        assert_eq!(recipe["data"]["brewing"]["grinder_enabled"], false);
    }
}
