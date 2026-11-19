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
    peers,
    protocol::{
        API_VERSION, BindRequest, BoundResponse, CreatePairingResponse, NetworkError, PAIRING_ALPN,
        PairingResponse, PairingStatus, Peer, QrPayload,
    },
};

const INVITATION_LIFETIME: Duration = Duration::from_secs(300);

#[derive(Debug)]
struct Invitation {
    id: String,
    token_hash: [u8; 32],
    endpoint_id: String,
    status: PairingStatus,
    expires_at: u64,
    deadline: Instant,
    peer: Option<Peer>,
}

impl Invitation {
    fn expire(&mut self) {
        if self.status == PairingStatus::Pending
            && (Instant::now() >= self.deadline || now() >= self.expires_at)
        {
            self.status = PairingStatus::Expired;
        }
    }

    fn response(&self) -> PairingResponse {
        PairingResponse {
            api_version: API_VERSION.to_owned(),
            pairing_id: self.id.clone(),
            status: self.status,
            expires_at: self.expires_at,
            peer: self.peer.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct State {
    invitations: VecDeque<Invitation>,
    peers: Option<BTreeMap<String, Peer>>,
    connections: BTreeMap<usize, Connection>,
}

/// 邀请仅驻留内存；设备保存成功后才提交消费结果。
#[derive(Debug)]
pub(super) struct PairingService {
    path: Result<PathBuf, String>,
    state: Mutex<State>,
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
            state: Mutex::new(State::default()),
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
        for invitation in &mut state.invitations {
            invitation.expire();
            if invitation.status == PairingStatus::Pending {
                invitation.status = PairingStatus::Cancelled;
            }
        }
        state.invitations.push_back(Invitation {
            id: id.clone(),
            token_hash: Sha256::digest(token.as_bytes()).into(),
            endpoint_id,
            status: PairingStatus::Pending,
            expires_at,
            deadline: Instant::now() + INVITATION_LIFETIME,
            peer: None,
        });
        while state.invitations.len() > 128 {
            state.invitations.pop_front();
        }
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
        let invitation = state
            .invitations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(NetworkError::PairingNotFound)?;
        invitation.expire();
        if cancel {
            match invitation.status {
                PairingStatus::Completed => return Err(NetworkError::Completed),
                PairingStatus::Pending => invitation.status = PairingStatus::Cancelled,
                _ => {}
            }
        }
        Ok(invitation.response())
    }

    pub async fn invalidate(&self) {
        let mut state = self.state.lock().await;
        for invitation in &mut state.invitations {
            invitation.expire();
            if invitation.status == PairingStatus::Pending {
                invitation.status = PairingStatus::Cancelled;
            }
        }
    }

    pub async fn list(&self) -> Result<Vec<Peer>, NetworkError> {
        let mut state = self.state.lock().await;
        self.load(&mut state).await?;
        Ok(state.peers.as_ref().unwrap().values().cloned().collect())
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
            for invitation in &mut state.invitations {
                if invitation
                    .peer
                    .as_ref()
                    .is_some_and(|peer| peer.endpoint_id == id)
                {
                    invitation.status = PairingStatus::Cancelled;
                    invitation.peer = None;
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

    async fn save(&self, peers: &BTreeMap<String, Peer>) -> Result<(), NetworkError> {
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
            let index = state
                .invitations
                .iter()
                .position(|item| item.id == request.pairing_id)
                .ok_or(NetworkError::PairingNotFound)?;
            let invitation = &mut state.invitations[index];
            invitation.expire();
            let hash: [u8; 32] = Sha256::digest(request.pairing_token.as_bytes()).into();
            if !bool::from(invitation.token_hash.ct_eq(&hash)) {
                return Err(NetworkError::InvalidToken);
            }
            match invitation.status {
                PairingStatus::Expired => return Err(NetworkError::Expired),
                PairingStatus::Cancelled => return Err(NetworkError::Cancelled),
                PairingStatus::Completed => {
                    let peer = invitation.peer.as_ref().unwrap();
                    if peer.endpoint_id != remote.to_string() {
                        return Err(NetworkError::Consumed);
                    }
                    return Ok(BoundResponse {
                        api_version: API_VERSION.to_owned(),
                        kind: "pairing.bound".to_owned(),
                        endpoint_id: invitation.endpoint_id.clone(),
                        bound_at: peer.bound_at,
                    });
                }
                PairingStatus::Pending => {}
            }
            if invitation.endpoint_id == remote.to_string() {
                return Err(NetworkError::InvalidRequest);
            }
            let peer = Peer {
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
            let invitation = &mut state.invitations[index];
            invitation.status = PairingStatus::Completed;
            invitation.peer = Some(peer.clone());
            Ok(BoundResponse {
                api_version: API_VERSION.to_owned(),
                kind: "pairing.bound".to_owned(),
                endpoint_id: invitation.endpoint_id.clone(),
                bound_at: peer.bound_at,
            })
        })
        .await
        .map_err(|_| NetworkError::Storage)?
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
