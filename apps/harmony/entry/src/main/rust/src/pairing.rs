use crate::{
    error::{InvalidRequestReason, ResponsError},
    network::PAIR_ALPN,
};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub version: u32,
    pub pairing_id: String,
    pub endpoint_id: String,
    pub relay_url: String,
    pub relay_token: String,
    pub alpn: String,
    pub pairing_token: String,
    pub expires_at: u64,
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn valid_relay(address: &str, token: &str) -> bool {
    url::Url::parse(address).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    }) && !token.trim().is_empty()
        && token.len() <= 4096
}
impl Invitation {
    pub fn decode(payload: &str) -> Result<Self, ResponsError> {
        let invalid = || ResponsError::from(InvalidRequestReason::InvalidInvitation);
        if payload.len() > 16_384 {
            return Err(invalid());
        }
        let value: Self = serde_json::from_str(payload).map_err(|_| invalid())?;
        if value.version != 1
            || value.alpn.as_bytes() != PAIR_ALPN
            || value.endpoint_id.parse::<EndpointId>().is_err()
            || uuid::Uuid::parse_str(&value.pairing_id).is_err()
            || value.pairing_token.len() != 64
            || !value
                .pairing_token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !valid_relay(&value.relay_url, &value.relay_token)
        {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn parse(payload: &str) -> Result<Self, ResponsError> {
        let value = Self::decode(payload)?;
        if value.expires_at <= now() {
            return Err(InvalidRequestReason::ExpiredInvitation.into());
        }
        Ok(value)
    }
}
