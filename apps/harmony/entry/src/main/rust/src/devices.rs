use crate::{
    network::{ErrorKind, Network, NetworkError, RequestOptions, Service, ServiceRequest},
    storage::{self, Computer, Store},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex as AsyncMutex, oneshot};

type Result<T> = std::result::Result<T, String>;
#[derive(Deserialize)]
struct ComputerSession {
    api_version: String,
    endpoint_id: String,
    status: String,
    bound_at: u64,
}

pub struct DeviceManager {
    stopping: AtomicBool,
    network: Arc<Network>,
    state: Mutex<(PathBuf, Store)>,
    operation: AsyncMutex<()>,
}
impl DeviceManager {
    pub(crate) fn new(network: Arc<Network>, path: PathBuf, store: Store) -> Self {
        Self {
            stopping: AtomicBool::new(false),
            network,
            state: Mutex::new((path, store)),
            operation: AsyncMutex::new(()),
        }
    }
    pub fn network(&self) -> Arc<Network> {
        self.network.clone()
    }
    pub(crate) fn stop_accepting(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
    pub(crate) async fn wait_stopped(&self) {
        let _operation = self.operation.lock().await;
    }
    fn ensure_running(&self) -> Result<()> {
        if self.stopping.load(Ordering::SeqCst) {
            Err("工作空间已停止".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn restore(network: &Network, store: &Store) -> Result<()> {
        if let Some(computer) = store
            .computers
            .iter()
            .find(|c| Some(&c.endpoint_id) == store.selected_endpoint_id.as_ref())
        {
            network
                .set_service(Self::service(store, computer)?)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn service(store: &Store, computer: &Computer) -> Result<Service> {
        Service::new(
            store.secret_key.parse().map_err(|_| "手机身份无效")?,
            computer.endpoint_id.parse().map_err(|_| "电脑身份无效")?,
            computer.relay_url.clone(),
            computer.relay_token.clone(),
        )
        .map_err(|e| e.to_string())
    }
    fn list_locked(store: &Store) -> Vec<Value> {
        store
            .computers
            .iter()
            .map(|c| {
                let active = Some(&c.endpoint_id) == store.selected_endpoint_id.as_ref();
                view(
                    c,
                    if !active {
                        "inactive"
                    } else if c.bound_at.is_some() {
                        "unknown"
                    } else {
                        "pending"
                    },
                    if active {
                        "等待检查电脑状态"
                    } else {
                        "选择此电脑后检查状态"
                    },
                    0,
                    active,
                )
            })
            .collect()
    }
    pub fn list(&self) -> Value {
        if let Err(error) = self.ensure_running() {
            return json!({"ok":false,"error":error});
        }
        let state = self.state.lock().unwrap();
        json!({"ok":true,"devices":Self::list_locked(&state.1)})
    }
    pub async fn select(&self, id: &str) -> Result<Value> {
        let _operation = self.operation.lock().await;
        self.ensure_running()?;
        let mut state = self.state.lock().unwrap();
        let computer = state
            .1
            .computers
            .iter()
            .find(|c| c.endpoint_id == id)
            .ok_or("电脑记录不存在")?;
        let service = Self::service(&state.1, computer)?;
        state.1.selected_endpoint_id = Some(id.into());
        storage::save(&state.0, &state.1)?;
        self.network
            .set_service(service)
            .map_err(|e| e.to_string())?;
        Ok(json!({"ok":true}))
    }
    pub async fn bind(&self, payload: &str) -> Result<Value> {
        let _operation = self.operation.lock().await;
        self.ensure_running()?;
        let invitation = storage::parse_invitation(payload)?;
        let service = {
            let mut state = self.state.lock().unwrap();
            if invitation.endpoint_id == state.1.endpoint_id {
                return Err("不能绑定手机自身".into());
            }
            let old = state
                .1
                .computers
                .iter()
                .find(|c| c.endpoint_id == invitation.endpoint_id);
            let computer = Computer {
                endpoint_id: invitation.endpoint_id.clone(),
                relay_url: invitation.relay_url.clone(),
                relay_token: invitation.relay_token.clone(),
                bound_at: old.and_then(|c| c.bound_at),
                pending: Some(invitation.clone()),
            };
            let service = Self::service(&state.1, &computer)?;
            if let Some(index) = state
                .1
                .computers
                .iter()
                .position(|c| c.endpoint_id == computer.endpoint_id)
            {
                state.1.computers[index] = computer;
            } else {
                if state.1.computers.len() >= 64 {
                    return Err("最多可保存 64 台电脑".into());
                }
                state.1.computers.push(computer);
            }
            state.1.selected_endpoint_id = Some(invitation.endpoint_id.clone());
            storage::save(&state.0, &state.1)?;
            service
        };
        self.network
            .set_service(service)
            .map_err(|e| e.to_string())?;
        let service = self
            .network
            .selected_service()
            .await
            .map_err(|e| e.to_string())?;
        match service.pair(&invitation).await {
            Ok(value) => {
                let mut state = self.state.lock().unwrap();
                let computer = state
                    .1
                    .computers
                    .iter_mut()
                    .find(|c| c.endpoint_id == invitation.endpoint_id)
                    .unwrap();
                computer.bound_at = value["bound_at"].as_u64();
                computer.pending = None;
                storage::save(&state.0, &state.1)?;
                Ok(json!({"ok":true}))
            }
            Err(error) => Err(format!("{}；可刷新确认绑定结果或重新扫码", error.message)),
        }
    }
    async fn session(
        &self,
    ) -> std::result::Result<crate::network::ServiceResponse<ComputerSession>, NetworkError> {
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        let error_sender = sender.clone();
        self.network.request::<ComputerSession, _, _>(
            ServiceRequest::get("/network/session"),
            RequestOptions::default(),
            move |response| {
                if let Some(sender) = sender.lock().unwrap().take() {
                    let _ = sender.send(Ok(response));
                }
            },
            move |error| {
                if let Some(sender) = error_sender.lock().unwrap().take() {
                    let _ = sender.send(Err(error));
                }
            },
        );
        receiver
            .await
            .unwrap_or_else(|_| Err(NetworkError::new(ErrorKind::Closed)))
    }
    pub async fn refresh(&self) -> Result<Value> {
        let _operation = self.operation.lock().await;
        self.ensure_running()?;
        let (selected, invitation) = {
            let state = self.state.lock().unwrap();
            let Some(id) = state.1.selected_endpoint_id.clone() else {
                return Ok(json!({"ok":true,"devices":Self::list_locked(&state.1)}));
            };
            let invitation = state
                .1
                .computers
                .iter()
                .find(|c| c.endpoint_id == id)
                .and_then(|c| c.pending.clone());
            (id, invitation)
        };
        let mut response = self.session().await;
        if response.as_ref().is_err_and(|e| e.code == "peer-not-bound")
            && invitation
                .as_ref()
                .is_some_and(|i| i.expires_at > storage::now())
        {
            let service = self
                .network
                .selected_service()
                .await
                .map_err(|e| e.to_string())?;
            response = match service.pair(invitation.as_ref().unwrap()).await {
                Ok(_) => self.session().await,
                Err(error) => Err(error),
            };
        }
        self.ensure_running()?;
        let mut state = self.state.lock().unwrap();
        if state.1.selected_endpoint_id.as_ref() != Some(&selected) {
            return Err("绑定的电脑已切换".into());
        }
        let computer = state
            .1
            .computers
            .iter_mut()
            .find(|c| c.endpoint_id == selected)
            .unwrap();
        let current = match response {
            Ok(response) => {
                if response.generation != self.network.generation() {
                    return Err("绑定的电脑已切换".into());
                }
                let session = response.data;
                if session.api_version != "1"
                    || session.endpoint_id != selected
                    || session.status != "online"
                {
                    return Err("电脑返回的会话无效".into());
                }
                computer.bound_at = Some(session.bound_at);
                computer.pending = None;
                view(computer, "online", "电脑服务可用", storage::now(), true)
            }
            Err(error) => view(
                computer,
                if computer.pending.is_some() {
                    "pending"
                } else if error.code == "peer-not-bound" {
                    "unbound"
                } else {
                    "unreachable"
                },
                &error.message,
                storage::now(),
                true,
            ),
        };
        storage::save(&state.0, &state.1)?;
        let mut devices = Self::list_locked(&state.1);
        if let Some(index) = state
            .1
            .computers
            .iter()
            .position(|c| c.endpoint_id == selected)
        {
            devices[index] = current;
        }
        Ok(json!({"ok":true,"devices":devices}))
    }
}
fn view(computer: &Computer, status: &str, detail: &str, checked_at: u64, active: bool) -> Value {
    json!({"endpoint_id":computer.endpoint_id,"name":"已绑定电脑","bound_at":computer.bound_at,"status":status,"detail":detail,"checked_at":checked_at,"active":active})
}
