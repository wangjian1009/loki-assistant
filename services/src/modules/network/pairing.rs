use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use iroh::{EndpointId, SecretKey, endpoint::Connection};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::time::Instant;

use crate::process::Process;

use crate::modules::config::service::ConfigService;

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
#[derive(Clone, Debug)]
pub(super) struct PairingService {
    config: ConfigService,
    state: Process<State>,
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl PairingService {
    pub fn new(config: ConfigService) -> Self {
        Self {
            config,
            state: Process::start(State::default()),
        }
    }

    async fn load(&self, state: &mut State) -> Result<(), NetworkError> {
        if state.peers.is_none() {
            state.peers = Some(peers::load(&self.config).await?);
        }
        Ok(())
    }

    pub(super) async fn initialize(&self) -> Result<(), NetworkError> {
        let service = self.clone();
        self.state
            .with(move |state| Box::pin(async move { service.load(state).await }))
            .await
    }

    pub async fn create(
        &self,
        endpoint_id: String,
        relay_url: String,
        relay_token: String,
    ) -> Result<CreatePairingResponse, NetworkError> {
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    service.load(state).await?;
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
                    let expiry_state = service.state.downgrade();
                    let deadline = state.invitations.back().unwrap().deadline;
                    tokio::spawn(async move {
                        tokio::time::sleep_until(deadline).await;
                        if let Some(state) = expiry_state.upgrade() {
                            state.post(State::expire);
                        }
                    });
                    Ok(CreatePairingResponse {
                        api_version: API_VERSION.to_owned(),
                        pairing_id: id,
                        status: PairingStatus::Pending,
                        qr_payload,
                        qr_code_png_base64,
                    })
                })
            })
            .await
    }

    pub async fn query(&self, id: &str, cancel: bool) -> Result<PairingResponse, NetworkError> {
        let id = id.to_owned();
        self.state
            .call(move |state| {
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
            })
            .await
    }

    pub async fn invalidate(&self) {
        self.state
            .call(move |state| {
                state.expire();
                while let Some(invitation) = state.invitations.pop_front() {
                    state.finish(invitation, PairingStatus::Cancelled, None);
                }
            })
            .await
    }

    pub async fn list(&self) -> Result<Vec<Peer>, NetworkError> {
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    service.load(state).await?;
                    Ok(state
                        .peers
                        .as_ref()
                        .unwrap()
                        .values()
                        .map(StoredPeer::public)
                        .collect())
                })
            })
            .await
    }

    // 独立任务持有提交，避免请求取消发生在磁盘提交与内存更新之间。
    pub async fn remove(&self, endpoint: EndpointId) -> Result<(), NetworkError> {
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    service.load(state).await?;
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
            })
            .await
    }

    async fn save(&self, peers: &BTreeMap<String, StoredPeer>) -> Result<(), NetworkError> {
        peers::save(&self.config, peers.values().cloned().collect()).await
    }

    pub async fn bind(
        &self,
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
        self.state
            .with(move |state| {
                Box::pin(async move {
                    service.load(state).await?;
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
            })
            .await
    }

    pub async fn authenticate(&self, remote: EndpointId, token: &str) -> Result<(), NetworkError> {
        let token = token.to_owned();
        let service = self.clone();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    service.load(state).await?;
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
                    if token.len() != 64
                        || !bool::from(expected.0.as_bytes().ct_eq(token.as_bytes()))
                    {
                        return Err(NetworkError::InvalidBindingToken);
                    }
                    Ok(())
                })
            })
            .await
    }

    pub async fn track(&self, connection: Connection) {
        self.state
            .call(move |state| {
                state.connections.insert(connection.stable_id(), connection);
            })
            .await;
    }

    pub async fn untrack(&self, id: usize) {
        self.state
            .call(move |state| {
                state.connections.remove(&id);
            })
            .await;
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn expired_invitation_is_removed_without_a_later_request() {
        let manager = PairingService::new(ConfigService::from_registry());
        let created = manager
            .create(
                SecretKey::generate().public().to_string(),
                "https://relay.example/".into(),
                "test".into(),
            )
            .await
            .unwrap();
        assert_eq!(manager.state.call(|state| state.invitations.len()).await, 1);
        tokio::time::advance(INVITATION_LIFETIME).await;
        tokio::task::yield_now().await;
        assert!(
            manager
                .state
                .call(|state| state.invitations.is_empty())
                .await
        );
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
