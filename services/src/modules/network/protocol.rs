use serde::{Deserialize, Serialize};

pub const API_VERSION: &str = "1";
pub const PAIRING_ALPN: &[u8] = b"loki-assistant/pair/1";
pub const DEVICE_STATUS_ALPN: &[u8] = b"loki-assistant/device-status/1";
pub const SERVICES_ALPN: &[u8] = b"loki-assistant/services/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceStatusRequest {
    pub api_version: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingStatus {
    Pending,
    Completed,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub endpoint_id: String,
    pub name: String,
    pub bound_at: u64,
}

#[derive(Serialize, Deserialize)]
pub struct QrPayload {
    pub version: u32,
    pub pairing_id: String,
    pub endpoint_id: String,
    pub relay_url: String,
    pub relay_token: String,
    pub alpn: String,
    pub pairing_token: String,
    pub expires_at: u64,
}

#[derive(Serialize, Deserialize)]
pub struct CreatePairingResponse {
    pub api_version: String,
    pub pairing_id: String,
    pub status: PairingStatus,
    pub qr_payload: QrPayload,
    pub qr_code_png_base64: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PairingResponse {
    pub api_version: String,
    pub pairing_id: String,
    pub status: PairingStatus,
    pub expires_at: u64,
    pub peer: Option<Peer>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BindRequest {
    pub api_version: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub pairing_id: String,
    pub pairing_token: String,
    pub device_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BoundResponse {
    pub api_version: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub endpoint_id: String,
    pub bound_at: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("请求格式无效")]
    InvalidRequest,
    #[error("中继尚未就绪，请先在设置中配置并连接中继")]
    NotReady,
    #[error("绑定邀请不存在，请刷新二维码")]
    PairingNotFound,
    #[error("绑定邀请已过期，请刷新二维码")]
    Expired,
    #[error("绑定邀请已取消，请刷新二维码")]
    Cancelled,
    #[error("绑定令牌无效")]
    InvalidToken,
    #[error("绑定邀请已被其他设备使用")]
    Consumed,
    #[error("绑定已经完成，无法取消")]
    Completed,
    #[error("绑定设备不存在")]
    PeerNotFound,
    #[error("此手机尚未绑定或已被电脑解除绑定，请重新扫码")]
    PeerNotBound,
    #[error("设备记录无法读取或保存，请检查 ~/.assistant/peers.json")]
    Storage,
    #[error("绑定信息无法生成二维码，请检查中继地址和令牌长度")]
    QrCode,
}

impl NetworkError {
    pub fn status(&self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::InvalidToken => 403,
            Self::PeerNotBound => 403,
            Self::PairingNotFound | Self::PeerNotFound => 404,
            Self::Consumed | Self::Completed => 409,
            Self::Expired | Self::Cancelled => 410,
            Self::NotReady => 503,
            Self::Storage => 500,
            Self::QrCode => 422,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::NotReady => "network-not-ready",
            Self::PairingNotFound => "pairing-not-found",
            Self::Expired => "pairing-expired",
            Self::Cancelled => "pairing-cancelled",
            Self::InvalidToken => "invalid-pairing-token",
            Self::Consumed => "pairing-consumed",
            Self::Completed => "pairing-completed",
            Self::PeerNotFound => "peer-not-found",
            Self::PeerNotBound => "peer-not-bound",
            Self::Storage => "peer-storage-error",
            Self::QrCode => "pairing-qr-code-error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    #[default]
    Offline,
    Disconnected,
    Connected,
    Failed,
    Connecting,
}
