use std::{sync::Arc, time::Duration};

use axum::Router;

use crate::{
    modules::{
        alert::AlertService, config, control_center, discover, emacs, health, network,
        notification::NotificationService, password, service, status::StatusService,
    },
    runtime::RuntimeState,
};

pub struct ExtensionContext<'a> {
    pub alert: &'a Arc<AlertService>,
    pub config: &'a Arc<config::service::ConfigService>,
    pub notification: &'a Arc<NotificationService>,
    pub status: &'a Arc<StatusService>,
    pub password: &'a password::Module,
    pub emacs: &'a emacs::Module,
}

pub type ExtensionFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;

pub trait ServiceExtension: std::fmt::Debug + Send + Sync {
    fn config_handler(
        &self,
    ) -> Option<(&'static str, Arc<dyn config::service::ConfigChangeHandler>)>;
    fn router(&self) -> Router;
    fn config_notification_router(&self) -> Router {
        Router::new()
    }
    fn run(self: Arc<Self>, port: u16) -> ExtensionFuture<'static>;
    fn stop(&self) -> ExtensionFuture<'_>;
}

pub struct ExtensionRegistration {
    pub build: fn(&ExtensionContext<'_>) -> Arc<dyn ServiceExtension>,
}

#[linkme::distributed_slice]
pub static EXTENSIONS: [ExtensionRegistration];

/// 控制中心服务模块的组合根。
#[derive(Debug)]
pub struct Services {
    pub health: health::Module,
    pub discover: discover::Module,
    pub config: config::Module,
    pub service: service::Module,
    pub notification: Arc<NotificationService>,
    pub status: Arc<StatusService>,
    pub alert: Arc<AlertService>,
    pub emacs: emacs::Module,
    pub network: Arc<network::NetworkService>,
    pub password: password::Module,
    extensions: Vec<Arc<dyn ServiceExtension>>,
    _config_notification_router: Arc<Router>,
}

/// 与需求文档中的 CoreService 名称兼容的别名。
pub type CoreService = Services;

impl Services {
    pub fn build() -> Self {
        Self::with_config(
            config::service::ConfigService::from_registry(),
            NotificationService::new(),
        )
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let config_path = config::service::ConfigService::storage_path()?;
        let home = config_path
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or(std::io::Error::other("无法确定本机缓存目录"))?;
        let notification = NotificationService::load(home.join(".cache/assistant/notification"))?;
        crate::modules::slack::migrate_legacy_cache(&home.join(".cache"))?;
        Ok(Self::with_config(
            config::service::ConfigService::load(config_path)?,
            notification,
        ))
    }

    fn with_config(
        config: config::service::ConfigService,
        notification: NotificationService,
    ) -> Self {
        let health = health::Module::new();
        let discover = discover::Module::new();
        let notification = Arc::new(notification);
        let status = Arc::new(StatusService::new());
        let alert = Arc::new(AlertService::new(
            Arc::clone(&status),
            Arc::clone(&notification),
        ));
        let service = service::Module::new(Arc::clone(&status));
        let identity = config::service::ConfigService::storage_path()
            .map(|path| network::IdentityStore::new(path.with_file_name("identity.json")))
            .map_err(|error| error.to_string());
        let network = Arc::new(network::NetworkService::new(
            identity,
            Arc::new(network::IrohRelayRegistrar::new()),
            Arc::clone(&notification),
            Arc::clone(&status),
            Duration::from_secs(10),
        ));
        let password = password::Module::new(Arc::new(config.clone()));
        let config = config.with_password_service(password.service());
        let base_config = Arc::new(config.clone());
        let emacs = emacs::Module::new(
            Arc::clone(&base_config),
            Arc::clone(&notification),
            Arc::clone(&status),
        );
        let context = ExtensionContext {
            alert: &alert,
            config: &base_config,
            notification: &notification,
            status: &status,
            password: &password,
            emacs: &emacs,
        };
        let extensions: Vec<_> = EXTENSIONS
            .iter()
            .map(|entry| (entry.build)(&context))
            .collect();
        let mut config = config
            .with_change_handler("control-center", control_center::handler())
            .with_change_handler("network", network.clone());
        for extension in &extensions {
            if let Some((module, handler)) = extension.config_handler() {
                config = config.with_change_handler(module, handler);
            }
        }
        let config = config::Module::from_service(config);
        let config_notification_router =
            Arc::new(extensions.iter().fold(Router::new(), |router, extension| {
                router.merge(extension.config_notification_router())
            }));
        config
            .service()
            .attach_notifications(&config_notification_router);

        Self {
            health,
            service,
            discover,
            config,
            notification,
            status,
            alert,
            emacs,
            network,
            password,
            extensions,
            _config_notification_router: config_notification_router,
        }
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        let router = Router::new()
            .merge(self.health.router(runtime))
            .merge(self.discover.router())
            .merge(self.config.router())
            .merge(self.notification.router())
            .merge(self.status.router())
            .merge(self.alert.router())
            .merge(self.network.router())
            .merge(self.emacs.router())
            .merge(self.password.router());
        self.extensions
            .iter()
            .fold(router, |router, extension| router.merge(extension.router()))
    }

    pub fn start_extensions(&self, port: u16) -> Vec<tokio::task::JoinHandle<()>> {
        self.extensions
            .iter()
            .map(|extension| tokio::spawn(Arc::clone(extension).run(port)))
            .collect()
    }

    pub async fn stop_extensions(&self, tasks: Vec<tokio::task::JoinHandle<()>>) {
        for extension in &self.extensions {
            extension.stop().await;
        }
        for task in tasks {
            let _ = task.await;
        }
    }

    pub fn attach_remote(self: &Arc<Self>, runtime: &Arc<RuntimeState>) {
        let runtime = Arc::downgrade(runtime);
        self.network.set_remote_dispatcher(Arc::new(
            crate::modules::network::remote::HttpDispatcher::new(move || {
                let runtime = runtime.upgrade()?;
                Some(runtime.services().router(runtime))
            }),
        ));
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn publishers_and_http_readers_share_the_services_owned_instance() {
        let services = Arc::new(Services::build());
        // 网络构造和 Emacs 业务发布必须写到组合根持有的同一个实例。
        assert_eq!(
            services.status.value("network", "my").unwrap().value["status"],
            "offline"
        );
        assert!(
            services
                .status
                .value("service", "start_at")
                .unwrap()
                .value
                .as_u64()
                .unwrap()
                > 0
        );
        let runtime = json!({"pid":42,"version":"31.1","started_at":1,"reported_at":2});
        services
            .emacs
            .service()
            .receive_status(
                serde_json::from_value(json!({
                    "emacs": {"runtime": runtime.clone()}
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            services.status.value("emacs", "runtime").unwrap().value,
            runtime
        );

        let app = services.router(Arc::new(RuntimeState::new(Arc::clone(&services))));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status/emacs/runtime")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["value"], runtime);
        assert!(Services::build().status.value("emacs", "runtime").is_err());
    }
}
