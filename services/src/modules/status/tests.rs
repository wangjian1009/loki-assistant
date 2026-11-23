use super::StatusService;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

async fn request(
    service: Arc<StatusService>,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = service
        .router()
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
async fn rust_publishers_preserve_json_values_and_http_reads_the_shared_snapshot() {
    let service = Arc::new(StatusService::new());
    let (_, empty) = request(service.clone(), "GET", "/status", Value::Null).await;
    assert_eq!(
        empty,
        json!({"api_version":"1","status":{},"updated_at":null})
    );
    let values = json!({"emacs":{"runtime":{"pid":123}},
        "tools":{"enabled":false,"count":0,"empty":null,"list":[]}});
    let posted = service
        .publish_many(serde_json::from_value(values.clone()).unwrap())
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(&posted).unwrap()["status"], values);
    assert!(posted.updated_at.unwrap() > 0);
    let (_, value) = request(service.clone(), "GET", "/status/tools/empty", Value::Null).await;
    assert_eq!(value["value"], Value::Null);
    assert_eq!(value["module_id"], "tools");
    let (_, value) = request(service.clone(), "GET", "/status/tools/enabled", Value::Null).await;
    assert_eq!(value["value"], false);
    let (_, value) = request(
        service.clone(),
        "GET",
        "/status/emacs/runtime/pid",
        Value::Null,
    )
    .await;
    assert_eq!(value["path"], "runtime/pid");
    assert_eq!(value["value"], 123);
    let (_, read) = request(service, "GET", "/status", Value::Null).await;
    assert_eq!(read, serde_json::to_value(posted).unwrap());
}

#[tokio::test]
async fn rust_publishers_merge_items_and_reject_invalid_batches_atomically() {
    let service = Arc::new(StatusService::new());
    service
        .publish_many(
            serde_json::from_value(
                json!({"emacs":{"runtime":{"pid":123}},"tasks":{"count":2,"items":[1]}}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    service.publish("tasks", "items", json!([])).await.unwrap();
    assert_eq!(
        service.snapshot().await.status,
        serde_json::from_value(
            json!({"emacs":{"runtime":{"pid":123}},"tasks":{"count":2,"items":[]}})
        )
        .unwrap()
    );
    let before = serde_json::to_value(service.snapshot().await).unwrap();
    for input in [
        json!({}),
        json!({"emacs":{}}),
        json!({"emacs":{"":1}}),
        json!({"bad/app":{"value":1}}),
        json!({"valid":{"value":2}," ":{"value":3}}),
    ] {
        assert!(
            service
                .publish_many(serde_json::from_value(input).unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            serde_json::to_value(service.snapshot().await).unwrap(),
            before
        );
    }
    let (status, body) = request(service, "GET", "/status/emacs/missing", Value::Null).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "status-not-found");
}

#[tokio::test]
async fn status_http_is_read_only_and_cannot_overwrite_rust_published_values() {
    let service = Arc::new(StatusService::new());
    service
        .publish("network", "my", json!({"status":"disconnected"}))
        .await
        .unwrap();
    service
        .publish("emacs", "runtime", json!({"pid":42}))
        .await
        .unwrap();
    for (method, path) in [
        ("POST", "/stats/meta"),
        ("POST", "/status"),
        ("PUT", "/status/network/my"),
        ("POST", "/status/emacs/runtime"),
    ] {
        let response = service
            .router()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("null"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
    let (_, value) = request(service.clone(), "GET", "/status/emacs/runtime", Value::Null).await;
    assert_eq!(value["value"], json!({"pid":42}));
    assert_eq!(
        service.value("network", "my").await.unwrap().value,
        json!({"status":"disconnected"})
    );
    assert!(
        StatusService::new()
            .value("emacs", "runtime")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn status_module_is_discoverable_and_old_emacs_status_routes_are_removed() {
    let app = crate::http::router(Arc::new(crate::runtime::RuntimeState::default()));
    for path in [
        "/emacs/status",
        "/emacs/status/emacs/buffer",
        "/status/emacs/buffers",
        "/network/status/my",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    for path in [
        "/discover/modules/status",
        "/status/network/my",
        "/status/network/my/status",
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        if path.starts_with("/discover") {
            assert_eq!(body["apis"].as_array().unwrap().len(), 5);
            assert!(
                body["apis"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|api| api["method"] == "GET")
            );
        } else if path.ends_with("/status") {
            assert_eq!(body["path"], "my/status");
            assert_eq!(body["value"], "offline");
        } else {
            assert_eq!(body["value"]["status"], "offline");
        }
    }
}

#[tokio::test]
async fn service_state_has_no_node_layer_and_module_query_returns_all_fields() {
    let services = Arc::new(crate::Services::build());
    let status = Arc::clone(&services.status);
    let snapshot = status.snapshot().await;
    let start_at = snapshot.status["service"]["start_at"].as_u64().unwrap();
    assert!(start_at > 0);
    assert_eq!(snapshot.status["service"].len(), 3);
    assert!(
        !snapshot.status["service"]["name"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(snapshot.status["service"]["device_type"], "computer");
    let (code, body) = request(Arc::clone(&status), "GET", "/status/service", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(body["module_id"], "service");
    assert_eq!(body["value"], json!(snapshot.status["service"]));
    assert!(body["updated_at"].as_u64().unwrap() >= start_at);
    let (code, _) = request(
        Arc::clone(&status),
        "GET",
        "/status/service/my",
        Value::Null,
    )
    .await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (code, _) = request(status, "GET", "/status/device", Value::Null).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_publishers_preserve_independent_items() {
    let service = Arc::new(StatusService::new());
    let mut publishers = tokio::task::JoinSet::new();
    for index in 0..12 {
        let service = service.clone();
        publishers.spawn(async move {
            service
                .publish("test", &format!("item{index}"), json!(index))
                .await
                .unwrap();
        });
    }
    while let Some(result) = publishers.join_next().await {
        result.unwrap();
    }
    assert_eq!(service.snapshot().await.status["test"].len(), 12);
    for name in ["", " ", ".", "..", "bad/name"] {
        assert!(service.publish(name, "value", json!(1)).await.is_err());
        assert!(service.publish("test", name, json!(1)).await.is_err());
    }
    assert_eq!(service.snapshot().await.status["test"].len(), 12);
}

#[tokio::test]
async fn metadata_is_complete_before_publication_and_contains_no_runtime_values() {
    let service = Arc::new(StatusService::new());
    let (code, metadata) = request(service.clone(), "GET", "/stats/meta", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(metadata["api_version"], "1");
    let modules = metadata["modules"].as_array().unwrap();
    for id in ["emacs", "network", "service"] {
        assert!(modules.iter().any(|module| module["id"] == id));
    }
    assert!(service.snapshot().await.status.is_empty());
    assert_eq!(service.snapshot().await.updated_at, None);
    service
        .publish(
            "custom",
            "value",
            json!({"private_runtime":"仅存于数据接口"}),
        )
        .await
        .unwrap();
    let (_, reread) = request(service, "GET", "/stats/meta", Value::Null).await;
    assert_eq!(reread, metadata);
    assert!(!reread.to_string().contains("private_runtime"));
}

#[tokio::test]
async fn metadata_describes_emacs_runtime_and_all_network_states() {
    use crate::modules::network::protocol::ConnectionStatus;
    let service = Arc::new(StatusService::new());
    let (_, metadata) = request(service, "GET", "/stats/meta", Value::Null).await;
    let modules = metadata["modules"].as_array().unwrap();
    let emacs = modules
        .iter()
        .find(|module| module["id"] == "emacs")
        .unwrap();
    let items = emacs["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let runtime = items.iter().find(|item| item["key"] == "runtime").unwrap();
    assert_eq!(runtime["schema"]["data_type"], "object");
    assert_eq!(runtime["schema"]["fields"].as_array().unwrap().len(), 4);
    let network = modules
        .iter()
        .find(|module| module["id"] == "network")
        .unwrap();
    let my = network["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == "my")
        .unwrap();
    let status = &my["schema"]["fields"][0];
    assert_eq!(status["key"], "status");
    assert_eq!(status["schema"]["data_type"], "string");
    let choices = status["schema"]["choices"].as_array().unwrap();
    let actual: Vec<Value> = choices
        .iter()
        .map(|choice| choice["value"].clone())
        .collect();
    let expected: Vec<Value> = [
        ConnectionStatus::Offline,
        ConnectionStatus::Disconnected,
        ConnectionStatus::Connected,
        ConnectionStatus::Failed,
        ConnectionStatus::Connecting,
    ]
    .into_iter()
    .map(|status| serde_json::to_value(status).unwrap())
    .collect();
    assert_eq!(actual, expected);
    assert!(
        choices
            .iter()
            .all(|choice| !choice["label"].as_str().unwrap().is_empty())
    );
    let service_module = modules
        .iter()
        .find(|module| module["id"] == "service")
        .unwrap();
    let service_item = service_module["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["key"] == "start_at")
        .unwrap();
    assert_eq!(service_item["schema"]["data_type"], "integer");
}

#[tokio::test]
async fn external_validation_checks_primitive_types_optional_fields_and_nullable_values() {
    use super::{
        model::{StatusFieldMetadata as Field, StatusSchema},
        validation::validate,
    };
    for (schema, accepted, rejected) in [
        (StatusSchema::Integer, json!(42), json!(42.5)),
        (StatusSchema::Number, json!(42.5), json!("42.5")),
        (StatusSchema::Boolean, json!(false), json!("false")),
        (StatusSchema::Null, Value::Null, json!(false)),
        (StatusSchema::string(), json!("中文"), Value::Null),
    ] {
        validate(&schema, &accepted, "test/value").unwrap();
        assert!(validate(&schema, &rejected, "test/value").is_err());
    }
    validate(
        &StatusSchema::Any,
        &json!({"free":[null,false,42]}),
        "test/value",
    )
    .unwrap();
    let service = StatusService::new();
    for state in [
        "offline",
        "disconnected",
        "connected",
        "failed",
        "connecting",
    ] {
        service
            .publish_many_validated(
                serde_json::from_value(json!({"network":{"my":{"status":state}}})).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            service.value("network", "my").await.unwrap().value["status"],
            state
        );
    }
    let mut optional = Field::new(
        "optional",
        "可选值",
        "可省略但存在时必须是 boolean。",
        StatusSchema::Boolean,
    );
    optional.required = false;
    let schema = StatusSchema::Object {
        fields: vec![
            Field::new("file", "文件", "可为 null。", StatusSchema::string()).nullable(),
            optional,
        ],
    };
    validate(&schema, &json!({"file":null}), "test/object").unwrap();
    for (value, path) in [
        (json!({}), "test/object.file"),
        (json!({"file":false}), "test/object.file"),
        (json!({"file":null,"optional":null}), "test/object.optional"),
        (json!({"file":null,"extra":1}), "test/object.extra"),
    ] {
        assert!(
            validate(&schema, &value, "test/object")
                .unwrap_err()
                .to_string()
                .contains(path)
        );
    }
    let array = StatusSchema::Array {
        items: Box::new(StatusSchema::Boolean),
    };
    validate(&array, &json!([]), "test/array").unwrap();
    assert!(
        validate(&array, &json!([true, "false"]), "test/array")
            .unwrap_err()
            .to_string()
            .contains("test/array[1]")
    );
}
