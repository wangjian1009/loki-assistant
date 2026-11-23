use std::{future::pending, sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::timeout};
use tower::ServiceExt;

use super::{AlertInput, AlertLevel, AlertService};
use crate::{
    modules::{notification::NotificationService, status::StatusService},
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
    runtime::RuntimeState,
};

#[derive(Debug)]
struct BlockingLauncher(mpsc::UnboundedSender<NotificationContent>);

impl NotificationLauncher for BlockingLauncher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        self.0.send(content).unwrap();
        Box::pin(pending())
    }
}

#[tokio::test]
async fn rust_and_http_share_alert_state_and_trigger_background_notifications() {
    let (sent, mut notifications) = mpsc::unbounded_channel();
    let notification = Arc::new(NotificationService::with_launcher(
        Arc::new(BlockingLauncher(sent)),
        Duration::from_secs(30),
    ));
    let status = Arc::new(StatusService::new());
    let alerts = Arc::new(AlertService::new(status.clone(), notification));
    let app = alerts.router().merge(status.router());
    let endpoint = "/alert/backup.disk-space";

    let (code, first) = timeout(
        Duration::from_secs(2),
        request(
            &app,
            "PUT",
            endpoint,
            json!({"title":"磁盘空间不足","app":"backup"}),
        ),
    )
    .await
    .unwrap();
    assert_eq!(code, StatusCode::CREATED);
    let created = &first["alert"];
    assert_eq!(first, json!({"api_version":"1","alert":created}));
    assert_eq!(created["level"], "warning");
    let sent = timeout(Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent.app.as_deref(), Some("backup"));

    let (code, repeated) = request(
        &app,
        "PUT",
        endpoint,
        json!({"title":"磁盘空间不足","app":"backup","body":null,"level":"warning"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(repeated, first);

    let updated = alerts
        .publish(
            "backup.disk-space",
            AlertInput {
                app: None,
                title: "磁盘空间不足".into(),
                body: None,
                callback_url: None,
                level: AlertLevel::Warning,
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.app, None);
    assert_eq!(json!(updated.created_at), created["created_at"]);
    let sent = timeout(Duration::from_secs(2), notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent.app, None);
    assert_eq!(sent.title, "磁盘空间不足");

    let (_, current) = request(&app, "GET", "/status/alert/active", Value::Null).await;
    assert_eq!(current["value"], json!([updated]));

    for _ in 0..2 {
        let (code, body) = request(&app, "DELETE", endpoint, Value::Null).await;
        assert_eq!(code, StatusCode::NO_CONTENT);
        assert_eq!(body, Value::Null);
    }
    assert!(!alerts.clear("backup.disk-space").await.unwrap());
    let (_, cleared) = request(&app, "GET", "/status/alert/active", Value::Null).await;
    assert_eq!(cleared["value"], json!([]));
    tokio::task::yield_now().await;
    assert!(matches!(
        notifications.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn composed_services_register_alert_routes_and_keep_invalid_requests_out_of_status() {
    let runtime = Arc::new(RuntimeState::default());
    let app = runtime.services().router(runtime);
    let (code, module) = request(&app, "GET", "/discover/modules/alert", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(module["id"], "alert");
    assert_eq!(module["apis"].as_array().unwrap().len(), 2);
    assert!(
        module["apis"]
            .as_array()
            .unwrap()
            .iter()
            .all(|api| api["access"] == "local")
    );

    let (code, before) = request(&app, "GET", "/status/alert/active", Value::Null).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(before["value"], json!([]));
    for (path, input) in [
        ("/alert/backup%2Fdisk", json!({"title":"告警"})),
        ("/alert/backup.disk", json!({"title":"  "})),
        ("/alert/backup.disk", json!({"title":"告警","app":"Backup"})),
        (
            "/alert/backup.disk",
            json!({"title":"告警","id":"override"}),
        ),
        ("/alert/backup.disk", json!({"title":"告警","level":null})),
        (
            "/alert/backup.disk",
            json!({"title":"告警","callback_url":"https://example.org/reset"}),
        ),
    ] {
        let (code, body) = request(&app, "PUT", path, input).await;
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid-request");
    }
    let (_, after) = request(&app, "GET", "/status/alert/active", Value::Null).await;
    assert_eq!(after, before);
}

async fn request(app: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (code, body)
}
