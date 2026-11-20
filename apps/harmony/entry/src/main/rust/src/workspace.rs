use crate::{
    network::{
        AutoBinding, BindingState, InvalidRequestReason, Network, NetworkReason, RequestHandle,
        RequestOptions, ResponsError, Service, ServiceRequest, ServiceResponse,
    },
    storage,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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

struct Pending {
    cancelled: bool,
    handle: Option<RequestHandle>,
}

pub struct Workspace {
    network: Arc<Network>,
    storage: Mutex<(PathBuf, storage::Store)>,
    pending: Arc<Mutex<std::collections::BTreeMap<String, Pending>>>,
    runtime: Mutex<Option<Runtime>>,
    executor: tokio::runtime::Handle,
    stopping: AtomicBool,
    stop_gate: tokio::sync::Mutex<()>,
    stop_executor: Mutex<()>,
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
        Ok(Self {
            network,
            storage: Mutex::new((path, store)),
            pending: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            executor: runtime.handle().clone(),
            runtime: Mutex::new(Some(runtime)),
            stopping: AtomicBool::new(false),
            stop_gate: tokio::sync::Mutex::new(()),
            stop_executor: Mutex::new(()),
        })
    }
    pub fn network(&self) -> Arc<Network> {
        self.network.clone()
    }
    /// 停止新操作并主动取消当前目标的所有请求。
    pub fn begin_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.network.begin_shutdown();
    }
    pub async fn stop(&self) {
        let _stop = self.stop_gate.lock().await;
        self.begin_stop();
        self.network.shutdown().await;
    }
    pub async fn close(&self) {
        self.stop().await;
    }
    /// NAPI 工作线程使用的阻塞适配，所有操作复用本 Workspace。
    pub fn execute(&self, operation: &str, payload: &str) -> Value {
        if matches!(operation, "stop" | "close") {
            let _guard = self.stop_executor.lock().unwrap();
            if self.runtime.lock().unwrap().is_some() {
                self.executor.block_on(self.stop());
                let runtime = self.runtime.lock().unwrap().take();
                if let Some(runtime) = runtime {
                    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
                }
            }
            return json!({"ok":true});
        }
        if self.stopping.load(Ordering::SeqCst) {
            return failure(crate::error::CancelledReason::NetworkClosed.into());
        }
        match operation {
            "load_state" => {
                let storage = self.storage.lock().unwrap();
                json!({"ok":true,"state":storage.1.application,"endpoint_id":storage.1.endpoint_id})
            }
            "save_state" => {
                let data = serde_json::from_str::<serde_json::Map<String, Value>>(payload);
                match data {
                    Ok(data)
                        if !data.contains_key("secret_key")
                            && !data.contains_key("endpoint_id")
                            && !data.contains_key("version") =>
                    {
                        let mut storage = self.storage.lock().unwrap();
                        let mut candidate = storage.1.clone();
                        candidate.application = data;
                        match storage::save(&storage.0, &candidate) {
                            Ok(()) => {
                                storage.1 = candidate;
                                json!({"ok":true})
                            }
                            Err(error) => json!({"ok":false,"error":error}),
                        }
                    }
                    _ => json!({"ok":false,"error":"应用数据格式无效"}),
                }
            }
            "set_service" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Configuration {
                    endpoint_id: String,
                    relay_url: String,
                    relay_token: String,
                    binding: BindingState,
                    #[serde(default)]
                    auto_binding: Option<AutoBinding>,
                }
                let service = (|| -> Result<Service, ResponsError> {
                    let config: Configuration = serde_json::from_str(payload)
                        .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidService))?;
                    let key = self
                        .storage
                        .lock()
                        .unwrap()
                        .1
                        .secret_key
                        .parse()
                        .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidService))?;
                    let id = config
                        .endpoint_id
                        .parse()
                        .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidService))?;
                    let service = Service::new(key, id, config.relay_url, config.relay_token)?
                        .with_binding(config.binding);
                    Ok(if let Some(options) = config.auto_binding {
                        service.with_auto_binding(options)
                    } else {
                        service
                    })
                })();
                match service.and_then(|service| self.network.set_service(service)) {
                    Ok(()) => json!({"ok":true}),
                    Err(error) => failure(error),
                }
            }
            "clear_service" => {
                self.network.clear_service();
                json!({"ok":true})
            }
            "service" => json!({"ok":true,"service":self.network.service()}),
            "parse_invitation" => match crate::pairing::Invitation::decode(payload) {
                Ok(invitation) => json!({"ok":true,"invitation":invitation}),
                Err(error) => failure(error),
            },
            "request" => self.request_json(payload),
            "cancel" => {
                let mut pending = self.pending.lock().unwrap();
                let operation = pending.entry(payload.into()).or_insert(Pending {
                    cancelled: false,
                    handle: None,
                });
                operation.cancelled = true;
                if let Some(handle) = &operation.handle {
                    handle.cancel();
                }
                json!({"ok":true})
            }
            _ => json!({"ok":false,"error":"未知工作空间操作"}),
        }
    }
    fn request_json(&self, payload: &str) -> Value {
        let parsed = || -> Result<(ServiceRequest, RequestOptions), ResponsError> {
            let value: Value = serde_json::from_str(payload)
                .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidBody))?;
            let mut request = ServiceRequest::get(
                value["path"]
                    .as_str()
                    .ok_or_else(|| ResponsError::from(InvalidRequestReason::InvalidPath))?,
            );
            request.method = value["method"]
                .as_str()
                .ok_or_else(|| ResponsError::from(InvalidRequestReason::InvalidMethod))?
                .parse()
                .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidMethod))?;
            if let Some(query) = value.get("query") {
                request.query = serde_json::from_value(query.clone())
                    .map_err(|_| ResponsError::from(InvalidRequestReason::InvalidQuery))?;
            }
            request.body = value.get("body").cloned();
            let options = RequestOptions {
                timeout: std::time::Duration::from_millis(match value.get("timeout_ms") {
                    None => 10000,
                    Some(value) => value
                        .as_u64()
                        .ok_or_else(|| ResponsError::from(InvalidRequestReason::InvalidTimeout))?,
                }),
            };
            Ok((request, options))
        };
        let (request, options) = match parsed() {
            Ok(value) => value,
            Err(error) => return failure(error),
        };
        let operation_id = serde_json::from_str::<Value>(payload)
            .ok()
            .and_then(|value| value["operation_id"].as_str().map(str::to_owned));
        if let Some(id) = &operation_id {
            self.pending
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_insert(Pending {
                    cancelled: false,
                    handle: None,
                });
        }
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let error_sender = sender.clone();
        let success_pending = self.pending.clone();
        let error_pending = self.pending.clone();
        let success_id = operation_id.clone();
        let error_id = operation_id.clone();
        let handle = self.network.request::<Value, _, _>(
            request,
            options,
            move |response| {
                if let Some(id) = success_id {
                    success_pending.lock().unwrap().remove(&id);
                }
                let _ = sender.send(Ok(response));
            },
            move |error| {
                if let Some(id) = error_id {
                    error_pending.lock().unwrap().remove(&id);
                }
                let _ = error_sender.send(Err(error));
            },
        );
        if let Some(id) = operation_id {
            if let Some(pending) = self.pending.lock().unwrap().get_mut(&id) {
                if pending.cancelled {
                    handle.cancel();
                }
                pending.handle = Some(handle);
            }
        }
        response(receiver)
    }
}
fn failure(error: ResponsError) -> Value {
    json!({"ok":false,"respons_error":error})
}
fn response<T: serde::Serialize>(
    receiver: std::sync::mpsc::Receiver<Result<ServiceResponse<T>, ResponsError>>,
) -> Value {
    match receiver.recv() {
        Ok(Ok(response)) => {
            json!({"ok":true,"request_id":response.request_id.to_string(),"status":response.status.as_u16(),"body":response.data,"generation":response.generation})
        }
        Ok(Err(error)) => failure(error),
        Err(_) => failure(NetworkReason::InvalidResponse.into()),
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
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
