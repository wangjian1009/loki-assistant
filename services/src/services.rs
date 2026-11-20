use std::{sync::Arc, time::Duration};

use axum::Router;

use crate::{
    modules::{
        config, control_center, discover, emacs, health, network,
        notification::NotificationService, password, service, status::StatusService,
    },
    runtime::RuntimeState,
};

/// 控制中心服务模块的组合根。
#[derive(Debug)]
pub struct Services {
    pub health: health::Module,
    pub discover: discover::Module,
    pub config: config::Module,
    pub service: service::Module,
    pub notification: Arc<NotificationService>,
    pub status: Arc<StatusService>,
    pub emacs: emacs::Module,
    pub network: Arc<network::NetworkService>,
    pub password: password::Module,
}

/// 与需求文档中的 CoreService 名称兼容的别名。
pub type CoreService = Services;

impl Services {
    pub fn build() -> Self {
        Self::with_config(config::service::ConfigService::from_registry())
    }

    pub fn load() -> Result<Self, config::service::ConfigServiceError> {
        Ok(Self::with_config(config::service::ConfigService::load(
            config::service::ConfigService::storage_path()?,
        )?))
    }

    fn with_config(config: config::service::ConfigService) -> Self {
        let health = health::Module::new();
        let discover = discover::Module::new();
        let notification = Arc::new(NotificationService::new());
        let status = Arc::new(StatusService::new());
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
        let config = config::Module::from_service(
            config
                .with_change_handler("control-center", control_center::handler())
                .with_change_handler("network", network.clone()),
        );
        let emacs = emacs::Module::new(
            config.service(),
            Arc::clone(&notification),
            Arc::clone(&status),
        );
        let password = password::Module::new(config.service());

        Self {
            health,
            service,
            discover,
            config,
            notification,
            status,
            emacs,
            network,
            password,
        }
    }

    pub fn router(&self, runtime: Arc<RuntimeState>) -> Router {
        Router::new()
            .merge(self.health.router(runtime))
            .merge(self.discover.router())
            .merge(self.config.router())
            .merge(self.notification.router())
            .merge(self.status.router())
            .merge(self.network.router())
            .merge(self.emacs.router())
            .merge(self.password.router())
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
        let buffers = json!([{"name":"共享状态测试", "mode":"agent-shell-mode", "directory":"/tmp/", "file":null, "modified":false}]);
        services
            .emacs
            .service()
            .receive_status(
                serde_json::from_value(json!({
                    "emacs": {"buffers": buffers.clone()}
                }))
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            services.status.value("emacs", "buffers").unwrap().value,
            buffers
        );

        let app = services.router(Arc::new(RuntimeState::new(Arc::clone(&services))));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/status/emacs/buffers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["value"], buffers);
        assert!(Services::build().status.value("emacs", "buffers").is_err());
    }
}
