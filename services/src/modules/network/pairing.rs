use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::{EndpointId, SecretKey, endpoint::Connection};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::{sync::Mutex, time::Instant};

use super::{
    peers::{self, StoredPeer},
    protocol::{
        API_VERSION, BindRequest, BindingToken, BoundResponse, CreatePairingResponse, NetworkError,
        PAIRING_ALPN, PairingResponse, PairingStatus, Peer, QrPayload,
    },
};

const INVITATION_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct Invitation {
    id: String,
    token_hash: [u8; 32],
    endpoint_id: String,
    expires_at: u64,
    deadline: Instant,
}

#[derive(Debug, Default)]
struct State {
    invitations: VecDeque<Invitation>,
    // 只保留供本机查询的结果元数据；不保留邀请摘要或绑定 token。
    results: VecDeque<PairingResponse>,
    peers: Option<BTreeMap<String, StoredPeer>>,
    connections: BTreeMap<usize, Connection>,
}
impl State {
    fn finish(&mut self, invitation: Invitation, status: PairingStatus, peer: Option<Peer>) {
        self.results.push_back(PairingResponse {
            api_version: API_VERSION.to_owned(),
            pairing_id: invitation.id,
            status,
            expires_at: invitation.expires_at,
            peer,
        });
        while self.results.len() > 128 {
            self.results.pop_front();
        }
    }
    fn expire(&mut self) {
        let mut index = 0;
        while index < self.invitations.len() {
            let invitation = &self.invitations[index];
            if Instant::now() >= invitation.deadline || now() >= invitation.expires_at {
                let invitation = self.invitations.remove(index).unwrap();
                self.finish(invitation, PairingStatus::Expired, None);
            } else {
                index += 1;
            }
        }
    }
}

/// 邀请仅驻留内存；设备保存成功后才提交消费结果。
#[derive(Debug)]
pub(super) struct PairingService {
    path: Result<PathBuf, String>,
    state: Arc<Mutex<State>>,
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl PairingService {
    pub fn new(path: Result<PathBuf, String>) -> Self {
        Self {
            path,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    async fn load(&self, state: &mut State) -> Result<(), NetworkError> {
        if state.peers.is_none() {
            let path = self
                .path
                .as_ref()
                .map_err(|_| NetworkError::Storage)?
                .clone();
            state.peers = Some(
                tokio::task::spawn_blocking(move || peers::load(&path))
                    .await
                    .map_err(|_| NetworkError::Storage)??,
            );
        }
        Ok(())
    }

    pub async fn create(
        &self,
        endpoint_id: String,
        relay_url: String,
        relay_token: String,
    ) -> Result<CreatePairingResponse, NetworkError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await?;
        let token = SecretKey::generate()
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let id = uuid::Uuid::new_v4().to_string();
        let expires_at = now() + INVITATION_LIFETIME.as_secs();
        let qr_payload = QrPayload {
            version: 1,
            pairing_id: id.clone(),
            endpoint_id: endpoint_id.clone(),
            relay_url,
            relay_token,
            alpn: String::from_utf8_lossy(PAIRING_ALPN).into_owned(),
            pairing_token: token.clone(),
            expires_at,
        };
        let qr_code_png_base64 = super::qr_code::render(&qr_payload).await?;
        state.expire();
        while let Some(invitation) = state.invitations.pop_front() {
            state.finish(invitation, PairingStatus::Cancelled, None);
        }
        state.invitations.push_back(Invitation {
            id: id.clone(),
            token_hash: Sha256::digest(token.as_bytes()).into(),
            endpoint_id,
            expires_at,
            deadline: Instant::now() + INVITATION_LIFETIME,
        });
        let expiry_state = self.state.clone();
        let deadline = state.invitations.back().unwrap().deadline;
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            expiry_state.lock().await.expire();
        });
        Ok(CreatePairingResponse {
            api_version: API_VERSION.to_owned(),
            pairing_id: id,
            status: PairingStatus::Pending,
            qr_payload,
            qr_code_png_base64,
        })
    }

    pub async fn query(&self, id: &str, cancel: bool) -> Result<PairingResponse, NetworkError> {
        let mut state = self.state.lock().await;
        state.expire();
        if let Some(index) = state.invitations.iter().position(|item| item.id == id) {
            if cancel {
                let invitation = state.invitations.remove(index).unwrap();
                state.finish(invitation, PairingStatus::Cancelled, None);
            } else {
                let invitation = &state.invitations[index];
                return Ok(PairingResponse {
                    api_version: API_VERSION.to_owned(),
                    pairing_id: invitation.id.clone(),
                    status: PairingStatus::Pending,
                    expires_at: invitation.expires_at,
                    peer: None,
                });
            }
        }
        let result = state
            .results
            .iter()
            .find(|item| item.pairing_id == id)
            .ok_or(NetworkError::PairingNotFound)?;
        if cancel && result.status == PairingStatus::Completed {
            return Err(NetworkError::Completed);
        }
        Ok(PairingResponse {
            api_version: result.api_version.clone(),
            pairing_id: result.pairing_id.clone(),
            status: result.status,
            expires_at: result.expires_at,
            peer: result.peer.clone(),
        })
    }

    pub async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        state.expire();
        while let Some(invitation) = state.invitations.pop_front() {
            state.finish(invitation, PairingStatus::Cancelled, None);
        }
    }

    pub async fn list(&self) -> Result<Vec<Peer>, NetworkError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await?;
        Ok(state
            .peers
            .as_ref()
            .unwrap()
            .values()
            .map(StoredPeer::public)
            .collect())
    }

    // 独立任务持有提交，避免请求取消发生在磁盘提交与内存更新之间。
    pub async fn remove(self: &Arc<Self>, endpoint: EndpointId) -> Result<(), NetworkError> {
        let service = self.clone();
        tokio::spawn(async move {
            let mut state = service.state.lock().await;
            service.load(&mut state).await?;
            let id = endpoint.to_string();
            let mut peers = state.peers.as_ref().unwrap().clone();
            peers.remove(&id).ok_or(NetworkError::PeerNotFound)?;
            service.save(&peers).await?;
            state.peers = Some(peers);
            for result in &mut state.results {
                if result
                    .peer
                    .as_ref()
                    .is_some_and(|peer| peer.endpoint_id == id)
                {
                    result.status = PairingStatus::Cancelled;
                    result.peer = None;
                }
            }
            for connection in state
                .connections
                .values()
                .filter(|connection| connection.remote_id() == endpoint)
            {
                connection.close(0u32.into(), b"unbound");
            }
            Ok(())
        })
        .await
        .map_err(|_| NetworkError::Storage)?
    }

    async fn save(&self, peers: &BTreeMap<String, StoredPeer>) -> Result<(), NetworkError> {
        let path = self
            .path
            .as_ref()
            .map_err(|_| NetworkError::Storage)?
            .clone();
        let peers = peers.values().cloned().collect();
        tokio::task::spawn_blocking(move || peers::save(path, peers))
            .await
            .map_err(|_| NetworkError::Storage)?
    }

    pub async fn bind(
        self: &Arc<Self>,
        remote: EndpointId,
        request: BindRequest,
    ) -> Result<BoundResponse, NetworkError> {
        if request.api_version != API_VERSION
            || request.kind != "pairing.bind"
            || !peers::valid_name(&request.device_name)
        {
            return Err(NetworkError::InvalidRequest);
        }
        if request.pairing_token.len() != 64 {
            return Err(NetworkError::InvalidToken);
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut state = service.state.lock().await;
            service.load(&mut state).await?;
            state.expire();
            let index = state
                .invitations
                .iter()
                .position(|item| item.id == request.pairing_id)
                .ok_or(NetworkError::Expired)?;
            let invitation = &state.invitations[index];
            let hash: [u8; 32] = Sha256::digest(request.pairing_token.as_bytes()).into();
            if !bool::from(invitation.token_hash.ct_eq(&hash)) {
                return Err(NetworkError::InvalidToken);
            }
            if invitation.endpoint_id == remote.to_string() {
                return Err(NetworkError::InvalidRequest);
            }
            let peer = StoredPeer {
                device_type: request.device_type,
                binding_token: Some(BindingToken(
                    SecretKey::generate()
                        .to_bytes()
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                )),
                endpoint_id: remote.to_string(),
                name: request.device_name.trim().to_owned(),
                bound_at: now(),
            };
            let mut peers = state.peers.as_ref().unwrap().clone();
            peers.insert(peer.endpoint_id.clone(), peer.clone());
            if peers.len() > 256 {
                return Err(NetworkError::Storage);
            }
            service.save(&peers).await?;
            state.peers = Some(peers);
            let invitation = state.invitations.remove(index).unwrap();
            let endpoint_id = invitation.endpoint_id.clone();
            state.finish(invitation, PairingStatus::Completed, Some(peer.public()));
            Ok(BoundResponse {
                api_version: API_VERSION.to_owned(),
                kind: "pairing.bound".to_owned(),
                endpoint_id,
                bound_at: peer.bound_at,
                binding_token: peer.binding_token.unwrap(),
            })
        })
        .await
        .map_err(|_| NetworkError::Storage)?
    }

    pub async fn authenticate(&self, remote: EndpointId, token: &str) -> Result<(), NetworkError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await?;
        let peer = state
            .peers
            .as_ref()
            .unwrap()
            .get(&remote.to_string())
            .ok_or(NetworkError::PeerNotBound)?;
        let expected = peer
            .binding_token
            .as_ref()
            .ok_or(NetworkError::InvalidBindingToken)?;
        if token.len() != 64 || !bool::from(expected.0.as_bytes().ct_eq(token.as_bytes())) {
            return Err(NetworkError::InvalidBindingToken);
        }
        Ok(())
    }

    pub async fn track(&self, connection: Connection) {
        self.state
            .lock()
            .await
            .connections
            .insert(connection.stable_id(), connection);
    }

    pub async fn untrack(&self, id: usize) {
        self.state.lock().await.connections.remove(&id);
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn expired_invitation_is_removed_without_a_later_request() {
        let path = std::env::temp_dir().join(format!("loki-expiry-{}.json", uuid::Uuid::new_v4()));
        let manager = PairingService::new(Ok(path));
        let created = manager
            .create(
                SecretKey::generate().public().to_string(),
                "https://relay.example/".into(),
                "test".into(),
            )
            .await
            .unwrap();
        assert_eq!(manager.state.lock().await.invitations.len(), 1);
        tokio::time::advance(INVITATION_LIFETIME).await;
        tokio::task::yield_now().await;
        assert!(manager.state.lock().await.invitations.is_empty());
        assert_eq!(
            manager
                .query(&created.pairing_id, false)
                .await
                .unwrap()
                .status,
            PairingStatus::Expired
        );
    }
}
