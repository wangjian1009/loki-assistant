use crate::{
    devices::DeviceManager,
    network::{Network, NetworkError, RequestOptions, ServiceRequest},
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

pub struct Workspace {
    network: Arc<Network>,
    devices: DeviceManager,
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
        let mut store = storage::load(&path)?;
        if store.selected_endpoint_id.is_none() && store.computers.len() == 1 {
            store.selected_endpoint_id = Some(store.computers[0].endpoint_id.clone());
            storage::save(&path, &store)?;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| "无法启动工作空间")?;
        let network = {
            let _enter = runtime.enter();
            Arc::new(Network::new())
        };
        if let Err(error) = DeviceManager::restore(&network, &store) {
            runtime.shutdown_background();
            return Err(error);
        }
        let devices = DeviceManager::new(network.clone(), path, store);
        Ok(Self {
            network,
            devices,
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
    pub fn devices(&self) -> &DeviceManager {
        &self.devices
    }
    /// 先阻止业务接收新操作，再主动取消网络请求并等待业务收尾。
    pub async fn stop(&self) {
        let _stop = self.stop_gate.lock().await;
        self.stopping.store(true, Ordering::SeqCst);
        self.devices.stop_accepting();
        self.network.shutdown().await;
        self.devices.wait_stopped().await;
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
            return json!({"ok":false,"error":"工作空间已停止"});
        }
        let result = match operation {
            "list" => return self.devices.list(),
            "bind" => self.executor.block_on(self.devices.bind(payload)),
            "refresh" => self.executor.block_on(self.devices.refresh()),
            "select" => self.executor.block_on(self.devices.select(payload)),
            "request" => return self.request_json(payload),
            _ => Err("未知工作空间操作".into()),
        };
        result.unwrap_or_else(|error| json!({"ok":false,"error":error}))
    }
    fn request_json(&self, payload: &str) -> Value {
        let parsed = || -> Result<(ServiceRequest, RequestOptions), String> {
            let value: Value = serde_json::from_str(payload).map_err(|_| "请求 JSON 无效")?;
            let mut request = ServiceRequest::get(value["path"].as_str().ok_or("请求路径无效")?);
            request.method = value["method"]
                .as_str()
                .ok_or("请求方法无效")?
                .parse()
                .map_err(|_| "请求方法无效")?;
            if let Some(query) = value.get("query") {
                request.query =
                    serde_json::from_value(query.clone()).map_err(|_| "查询参数无效")?;
            }
            request.body = value.get("body").cloned();
            let options = RequestOptions {
                timeout: std::time::Duration::from_millis(
                    value["timeout_ms"].as_u64().unwrap_or(10000),
                ),
            };
            Ok((request, options))
        };
        let (request, options) = match parsed() {
            Ok(value) => value,
            Err(error) => return json!({"ok":false,"error":error}),
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let error_sender = sender.clone();
        self.network.request::<Value, _, _>(
            request,
            options,
            move |response| {
                let _ = sender.send(Ok(response));
            },
            move |error| {
                let _ = error_sender.send(Err(error));
            },
        );
        match receiver.recv() {
            Ok(Ok(response)) => {
                json!({"ok":true,"request_id":response.request_id.to_string(),"status":response.status.as_u16(),"body":response.data})
            }
            Ok(Err(error)) => json!({"ok":false,"error":error.message,"network_error":error}),
            Err(_) => {
                let error = NetworkError::new(crate::network::ErrorKind::Closed);
                json!({"ok":false,"error":error.message,"network_error":error})
            }
        }
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
