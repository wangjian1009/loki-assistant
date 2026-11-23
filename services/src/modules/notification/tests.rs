use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::{
    NotificationError, NotificationService,
    protocol::{NotificationReceipt, NotificationRequest, NotificationStatus},
    tracker::NotificationTracker,
};
use crate::{
    modules::{config::service::ConfigService, emacs},
    platform::macos::notification::{
        NotificationContent, NotificationFuture, NotificationLauncher,
    },
};

#[derive(Debug, Default)]
struct Launcher(Mutex<Vec<NotificationContent>>);

impl NotificationLauncher for Launcher {
    fn launch(&self, content: NotificationContent) -> NotificationFuture<'_> {
        self.0.lock().unwrap().push(content);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn code_interface_sends_without_emacs_or_callback_and_validates_navigation() {
    let launcher = Arc::new(Launcher::default());
    let service = NotificationService::with_launcher(launcher.clone(), Duration::from_secs(1));
    let result = service
        .send(NotificationRequest {
            title: "其他模块".to_owned(),
            body: Some("正文".to_owned()),
            app: None,
            callback_url: None,
        })
        .await
        .unwrap();
    assert_eq!(result.status, NotificationStatus::Unconfirmed);
    assert_eq!(launcher.0.lock().unwrap()[0].navigation_url, None);
    for callback in [
        "https://example.com/path",
        "//example.com/path",
        "/\\example.com",
        "/a#fragment",
    ] {
        assert!(matches!(
            service
                .send(NotificationRequest {
                    title: "无效跳转".to_owned(),
                    body: None,
                    app: None,
                    callback_url: Some(callback.to_owned())
                })
                .await,
            Err(NotificationError::InvalidRequest(_))
        ));
    }
    assert_eq!(launcher.0.lock().unwrap().len(), 1);
    for app in [
        "",
        "../emacs",
        "/tmp/icon.png",
        "file:icon",
        "Emacs",
        "图标",
    ] {
        assert!(matches!(
            service
                .send(NotificationRequest {
                    title: "无效应用".to_owned(),
                    body: None,
                    app: Some(app.to_owned()),
                    callback_url: None,
                })
                .await,
            Err(NotificationError::InvalidRequest(_))
        ));
    }
    assert_eq!(launcher.0.lock().unwrap().len(), 1);
}

async fn request(app: Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = app
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
async fn http_send_validates_input_and_forwards_through_the_shared_tracker() {
    let launcher = Arc::new(Launcher::default());
    let service = Arc::new(NotificationService::with_launcher(
        launcher.clone(),
        Duration::from_secs(1),
    ));
    let app = service.router();
    for body in [
        json!({"body":"缺少标题"}),
        json!({"title":" "}),
        json!({"title":"错误类型","body":42}),
        json!({"title":"无效应用","app":"../emacs"}),
        json!({"title":"外部回调","callback_url":"https://example.org/"}),
        json!({"title":"额外字段","notification_id":"自定义 ID"}),
    ] {
        let (status, result) = request(app.clone(), "POST", "/notification", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(result["error"]["code"], "invalid-request");
    }
    assert!(launcher.0.lock().unwrap().is_empty());

    let (status, sent) = request(
        app.clone(),
        "POST",
        "/notification",
        json!({"title":"接口通知","body":"正文","app":"emacs","callback_url":"/emacs/activate"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sent["api_version"], "1");
    assert_eq!(sent["status"], "unconfirmed");
    let id = sent["notification_id"].as_str().unwrap();
    {
        let calls = launcher.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let content = &calls[0];
        assert_eq!(content.notification_id, id);
        assert_eq!(content.title, "接口通知");
        assert_eq!(content.body.as_deref(), Some("正文"));
        assert_eq!(content.app.as_deref(), Some("emacs"));
        assert!(content.navigation_url.is_some());
    }
    let (status, received) = request(
        app,
        "POST",
        &format!("/notification/{id}/receipt"),
        json!({"status":"submitted"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(received["status"], "confirmed");
}

#[tokio::test]
async fn emacs_defaults_missing_app_and_forwards_explicit_app_without_icons() {
    let launcher = Arc::new(Launcher::default());
    let notification = Arc::new(NotificationService::with_launcher(
        launcher.clone(),
        Duration::from_secs(1),
    ));
    let emacs = emacs::Module::new(
        Arc::new(ConfigService::new(emacs::config::EmacsConfig::metadata())),
        notification,
        Arc::new(crate::modules::status::StatusService::new()),
    );
    for (body, expected) in [
        (json!({"title":"默认应用"}), "emacs"),
        (json!({"title":"默认应用", "app":null}), "emacs"),
        (json!({"title":"指定应用", "app":"tasks"}), "tasks"),
        (json!({"title":"旧图标", "icon":"/tmp/custom.png"}), "emacs"),
    ] {
        let (status, _) =
            request(emacs.router(), "POST", "/emacs/receiver/notification", body).await;
        assert_eq!(status, StatusCode::OK);
        let calls = launcher.0.lock().unwrap();
        let content = calls.last().unwrap();
        assert_eq!(content.app.as_deref(), Some(expected));
        let payload = serde_json::to_value(content).unwrap();
        assert_eq!(payload["app"], expected);
        assert!(payload.get("icon").is_none());
    }
    let (status, _) = request(
        emacs.router(),
        "POST",
        "/emacs/receiver/notification",
        json!({"title":"无效应用", "app":"../emacs"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(launcher.0.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn emacs_uses_injected_module_and_both_routes_share_receipts() {
    let launcher = Arc::new(Launcher::default());
    let notification = Arc::new(NotificationService::with_launcher(
        launcher.clone(),
        Duration::from_secs(1),
    ));
    let emacs = emacs::Module::new(
        Arc::new(ConfigService::new(emacs::config::EmacsConfig::metadata())),
        notification.clone(),
        Arc::new(crate::modules::status::StatusService::new()),
    );
    let app = Router::new()
        .merge(emacs.router())
        .merge(notification.router());
    let (status, sent) = request(
        app.clone(),
        "POST",
        "/emacs/receiver/notification",
        json!({"title":"默认应用通知"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = sent["result"]["notification_id"].as_str().unwrap();
    {
        let calls = launcher.0.lock().unwrap();
        assert_eq!(calls[0].app.as_deref(), Some("emacs"));
        let navigation =
            percent_encoding::percent_decode_str(calls[0].navigation_url.as_deref().unwrap())
                .decode_utf8()
                .unwrap();
        assert_eq!(
            navigation,
            "loki-assistant://notification/activate?callback=/emacs/activate"
        );
    }
    let (status, state) = request(
        app.clone(),
        "GET",
        &format!("/notification/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["status"], "unconfirmed");
    let (status, _) = request(
        app.clone(),
        "POST",
        &format!("/notification/{id}/receipt"),
        json!({"status":"submitted"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, state) = request(
        app,
        "GET",
        &format!("/emacs/receiver/notification/{id}"),
        Value::Null,
    )
    .await;
    assert_eq!(state["status"], "confirmed");
    assert_eq!(
        notification.status(id).await.unwrap().status,
        NotificationStatus::Confirmed
    );
    notification
        .receive_receipt(id, NotificationReceipt::Submitted {})
        .await
        .unwrap();
}

#[tokio::test]
async fn pending_content_survives_restart_and_is_removed_only_on_confirmation() {
    let directory =
        std::env::temp_dir().join(format!("loki-notification-{}", uuid::Uuid::new_v4()));
    let launcher = Arc::new(Launcher::default());
    let load = || {
        NotificationService::load_with_launcher(
            directory.clone(),
            launcher.clone(),
            Duration::from_secs(1),
        )
        .unwrap()
    };
    let service = load();
    let notification = service
        .send(NotificationRequest {
            title: "待确认".into(),
            body: Some("正文".into()),
            app: Some("slack".into()),
            callback_url: Some("/slack/conversations/TTEAM.CWORK/open".into()),
        })
        .await
        .unwrap();
    let id = &notification.notification_id;
    let path = directory.join(format!("{id}.json"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let state = service
        .receive_receipt(
            id,
            NotificationReceipt::Failed {
                code: "permission-denied".into(),
                message: "未授权".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(state.status, NotificationStatus::Unconfirmed);
    assert!(path.is_file());
    drop(service);

    let service = load();
    assert_eq!(
        service.status(id).await.unwrap().status,
        NotificationStatus::Unconfirmed
    );
    let worker = service.retry_pending();
    tokio::pin!(worker);
    tokio::select! {
        _ = &mut worker => unreachable!(),
        _ = async {
            while launcher.0.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        } => {}
    }
    {
        let sent = launcher.0.lock().unwrap();
        assert_eq!(sent[0].notification_id, sent[1].notification_id);
        assert_eq!(sent[0].body, sent[1].body);
        assert_eq!(sent[0].navigation_url, sent[1].navigation_url);
    }
    for _ in 0..2 {
        let state = service
            .receive_receipt(id, NotificationReceipt::Submitted {})
            .await
            .unwrap();
        assert_eq!(state.status, NotificationStatus::Confirmed);
        assert!(!path.exists());
    }
    assert!(matches!(
        load().status(id).await,
        Err(NotificationError::NotFound)
    ));
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test(start_paused = true)]
async fn tray_startup_wakes_worker_and_retries_only_unconfirmed_notifications() {
    let launcher = Arc::new(Launcher::default());
    let service = Arc::new(NotificationService::with_launcher(
        launcher.clone(),
        Duration::from_secs(1),
    ));
    let send = |title: &str| {
        service.send(NotificationRequest {
            title: title.into(),
            body: None,
            app: None,
            callback_url: None,
        })
    };
    let pending = send("待确认").await.unwrap();
    let confirmed = send("已确认").await.unwrap();
    service
        .receive_receipt(
            &confirmed.notification_id,
            NotificationReceipt::Submitted {},
        )
        .await
        .unwrap();

    let worker_service = service.clone();
    let worker = tokio::spawn(async move { worker_service.retry_pending().await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(launcher.0.lock().unwrap().len(), 2);

    let response = service
        .router()
        .oneshot(
            Request::post("/notification/retry-pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"api_version": "1", "status": "accepted"})
    );

    // 一秒内没有定时扫描，只有托盘就绪请求能触发转交。
    tokio::time::sleep(Duration::from_secs(1)).await;
    {
        let sent = launcher.0.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2].notification_id, pending.notification_id);
    }
    assert_eq!(
        service
            .status(&pending.notification_id)
            .await
            .unwrap()
            .status,
        NotificationStatus::Unconfirmed
    );
    worker.abort();
}

#[tokio::test]
async fn startup_retry_leaves_in_flight_delivery_to_its_current_attempt() {
    let tracker = NotificationTracker::default();
    let id = uuid::Uuid::new_v4();
    let content = NotificationContent {
        notification_id: id.to_string(),
        title: "正在转交".into(),
        app: None,
        body: None,
        navigation_url: None,
    };
    let attempt = tracker.register(content, id).await.unwrap();
    tracker.schedule_retry().await;
    assert!(tracker.due().await.is_empty());

    drop(attempt);
    tracker.schedule_retry().await;
    assert_eq!(tracker.due().await, [id]);
    assert!(tracker.retry(id).await.is_some());
    assert!(tracker.retry(id).await.is_none());
}
