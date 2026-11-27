use crate::{
    network::{AutoBinding, BindingState, InvalidRequestReason, Network, ResponsError, Service},
    storage,
};
use loki_service_proxy::{
    agent_shell::AgentShell,
    debug_http::{HttpDispatch, HttpRoutes, HttpServer},
    mail::Mail,
    service_proxy::{
        ComputerStatus, DeviceRefreshEvent, ServiceProxy, ServiceSnapshot, StatusSubscription,
    },
};
use serde_json::Value;
#[cfg(not(target_env = "ohos"))]
use serde_json::json;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::runtime::Runtime;

pub struct WorkspaceOptions {
    pub directory: PathBuf,
}
impl WorkspaceOptions {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

#[derive(Clone, PartialEq, serde::Serialize)]
pub struct DashboardUpdate {
    generation: u64,
    endpoint_id: Option<String>,
    checked_at: u64,
    error: Option<ResponsError>,
    status: Option<std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>>>,
    service: Option<ComputerStatus>,
}

impl From<&ServiceSnapshot> for DashboardUpdate {
    fn from(snapshot: &ServiceSnapshot) -> Self {
        Self {
            generation: snapshot.generation,
            endpoint_id: snapshot.endpoint_id.clone(),
            checked_at: snapshot.checked_at,
            error: snapshot.error.clone(),
            status: snapshot.status.clone(),
            service: ComputerStatus::from_snapshot(snapshot),
        }
    }
}

#[derive(serde::Serialize)]
pub struct ApplicationData {
    pub state: serde_json::Map<String, Value>,
    pub endpoint_id: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfiguration {
    pub endpoint_id: String,
    pub relay_url: String,
    pub relay_token: String,
    pub binding: BindingState,
    #[serde(default)]
    pub auto_binding: Option<AutoBinding>,
}

pub struct Workspace {
    network: Arc<Network>,
    proxy: Arc<ServiceProxy>,
    mail: Mail,
    storage: Mutex<(PathBuf, storage::Store)>,
    runtime: Mutex<Option<Runtime>>,
    executor: tokio::runtime::Handle,
    stopping: AtomicBool,
    stop_gate: tokio::sync::Mutex<()>,
    stop_executor: Mutex<()>,
    http: Mutex<Option<HttpServer>>,
    http_routes: Arc<HttpRoutes>,
}
impl Workspace {
    /// 初始化只读取本机存储，不连接预设服务器；统一持有执行环境。
    pub fn open(options: WorkspaceOptions) -> Result<Self, String> {
        let path = options.directory.join("devices.json");
        let store = storage::load(&path)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "无法启动工作空间")?;
        let network = {
            let _enter = runtime.enter();
            Arc::new(Network::new())
        };
        let proxy = ServiceProxy::new(network.clone(), runtime.handle().clone());
        let mail = Mail::new(proxy.clone());
        Ok(Self {
            network,
            proxy,
            mail,
            storage: Mutex::new((path, store)),
            executor: runtime.handle().clone(),
            runtime: Mutex::new(Some(runtime)),
            stopping: AtomicBool::new(false),
            stop_gate: tokio::sync::Mutex::new(()),
            stop_executor: Mutex::new(()),
            http: Mutex::new(None),
            http_routes: Arc::new(HttpRoutes::default()),
        })
    }
    pub fn network(&self) -> Arc<Network> {
        self.network.clone()
    }
    pub fn load_state(&self) -> ApplicationData {
        let store = self.storage.lock().unwrap();
        ApplicationData {
            state: store.1.application.clone(),
            endpoint_id: store.1.endpoint_id.clone(),
        }
    }

    pub fn save_state(&self, state: serde_json::Map<String, Value>) -> Result<(), String> {
        if ["secret_key", "endpoint_id", "version"]
            .iter()
            .any(|key| state.contains_key(*key))
        {
            return Err("应用数据格式无效".into());
        }
        let mut store = self.storage.lock().unwrap();
        let mut candidate = store.1.clone();
        candidate.application = state;
        storage::save(&store.0, &candidate)?;
        store.1 = candidate;
        Ok(())
    }

    pub fn set_service(&self, config: ServiceConfiguration) -> Result<(), ResponsError> {
        let key = self
            .storage
            .lock()
            .unwrap()
            .1
            .secret_key
            .parse()
            .map_err(|_| InvalidRequestReason::InvalidService)?;
        let id = config
            .endpoint_id
            .parse()
            .map_err(|_| InvalidRequestReason::InvalidService)?;
        let mut service = Service::new(key, id, config.relay_url, config.relay_token)?
            .with_binding(config.binding);
        if let Some(options) = config.auto_binding {
            service = service.with_auto_binding(options);
        }
        self.network.set_service(service)?;
        self.proxy.sync_target();
        Ok(())
    }

    pub fn clear_service(&self) {
        self.network.clear_service();
        self.proxy.sync_target();
    }

    pub fn service(&self) -> Option<crate::network::ServiceState> {
        self.network.service()
    }

    pub fn parse_invitation(
        &self,
        payload: &str,
    ) -> Result<crate::pairing::Invitation, ResponsError> {
        crate::pairing::Invitation::decode(payload)
    }

    pub fn refresh_device(&self, endpoint: &str) -> Result<(), ResponsError> {
        self.proxy.refresh_device(Some(endpoint))
    }

    pub fn watch_refresh(
        &self,
        changed: impl FnMut(DeviceRefreshEvent) -> bool + Send + 'static,
    ) -> StatusSubscription {
        self.proxy.watch_refresh(changed)
    }

    pub fn open_mail(&self) -> Mail {
        self.mail.clone()
    }

    pub fn open_agent_shell(&self) -> AgentShell {
        AgentShell::new(self.proxy.clone())
    }

    pub fn open_slack(&self) -> loki_service_proxy::slack::Slack {
        loki_service_proxy::slack::Slack::new(self.proxy.clone())
    }

    pub fn register_http_route(
        &self,
        path: &str,
        methods: &[String],
        dispatch: Box<HttpDispatch>,
    ) -> Result<(), String> {
        let _server = self.http.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            return Err("工作空间已停止".into());
        }
        self.http_routes.register(path, methods, dispatch)
    }

    /// 使用当前工作空间的 Network 监听设备回环地址，端口 0 由系统分配。
    pub fn start_http_server(&self, port: u16) -> Result<SocketAddr, String> {
        let _guard = self.stop_executor.lock().unwrap();
        let mut server = self.http.lock().unwrap();
        if self.stopping.load(Ordering::SeqCst) {
            return Err("工作空间已停止".into());
        }
        if let Some(active) = server.as_ref().filter(|server| server.is_running()) {
            return Ok(active.address);
        }
        let started = HttpServer::start(
            &self.executor,
            self.network(),
            self.proxy.clone(),
            port,
            self.http_routes.clone(),
        )
        .map_err(|error| format!("无法启动调试 HTTP 服务：{error}"))?;
        let address = started.address;
        *server = Some(started);
        Ok(address)
    }

    pub fn respond_http(&self, id: u64, status: u16, body: &str) {
        if let Some(server) = self.http.lock().unwrap().as_ref() {
            server.respond(id, status, body);
        }
    }

    pub async fn stop_http_server(&self) {
        let server = self.http.lock().unwrap().take();
        if let Some(server) = server {
            server.stop().await;
        }
    }
    /// 停止新操作并主动取消当前目标的所有请求。
    pub fn begin_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.network.begin_shutdown();
        let mut http = self.http.lock().unwrap();
        self.http_routes.clear();
        if let Some(server) = http.as_mut() {
            server.begin_stop();
        }
    }
    pub async fn stop(&self) {
        let _stop = self.stop_gate.lock().await;
        self.begin_stop();
        self.stop_http_server().await;
        self.network.shutdown().await;
    }
    pub async fn close(&self) {
        self.stop().await;
    }
    pub fn shutdown(&self) {
        self.begin_stop();
        let _guard = self.stop_executor.lock().unwrap();
        if self.runtime.lock().unwrap().is_some() {
            self.executor.block_on(self.stop());
            if let Some(runtime) = self.runtime.lock().unwrap().take() {
                runtime.shutdown_timeout(Duration::from_secs(2));
            }
        }
    }

    #[cfg(not(target_env = "ohos"))]
    pub fn execute(&self, operation: &str, payload: &str) -> Value {
        if matches!(operation, "stop" | "close") {
            self.shutdown();
            return json!({"ok":true});
        }
        if operation == "stop_http" {
            self.executor.block_on(self.stop_http_server());
            return json!({"ok":true});
        }
        if self.stopping.load(Ordering::SeqCst) {
            return failure(crate::error::CancelledReason::NetworkClosed.into());
        }
        match operation {
            "load_state" => {
                let data = self.load_state();
                json!({"ok":true,"state":data.state,"endpoint_id":data.endpoint_id})
            }
            "save_state" => match serde_json::from_str(payload)
                .map_err(|_| "应用数据格式无效".to_owned())
                .and_then(|state| self.save_state(state))
            {
                Ok(()) => json!({"ok":true}),
                Err(error) => json!({"ok":false,"error":error}),
            },
            "set_service" => match serde_json::from_str(payload)
                .map_err(|_| InvalidRequestReason::InvalidService.into())
                .and_then(|config| self.set_service(config))
            {
                Ok(()) => json!({"ok":true}),
                Err(error) => failure(error),
            },
            "clear_service" => {
                self.clear_service();
                json!({"ok":true})
            }
            "service" => json!({"ok":true,"service":self.network.service()}),
            "parse_invitation" => match crate::pairing::Invitation::decode(payload) {
                Ok(invitation) => json!({"ok":true,"invitation":invitation}),
                Err(error) => failure(error),
            },
            _ => json!({"ok":false,"error":"未知工作空间操作"}),
        }
    }
    pub fn watch_status(
        &self,
        notify: impl FnMut(DashboardUpdate) -> bool + Send + 'static,
    ) -> loki_service_proxy::service_proxy::StatusSubscription {
        self.proxy
            .subscribe(|snapshot| DashboardUpdate::from(snapshot), notify)
    }
}

#[cfg(not(target_env = "ohos"))]
fn failure(error: ResponsError) -> Value {
    json!({"ok":false,"respons_error":error})
}
impl Drop for Workspace {
    fn drop(&mut self) {
        self.http.get_mut().unwrap().take();
        if let Some(runtime) = self.runtime.get_mut().unwrap().take() {
            let network = self.network.clone();
            // 不在 Tokio 执行线程中阻塞销毁运行时。
            std::thread::spawn(move || {
                runtime.block_on(network.shutdown());
                runtime.shutdown_timeout(std::time::Duration::from_secs(2));
            });
        }
    }
}
