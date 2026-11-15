use serde::Serialize;

use crate::runtime::RuntimeState;

pub const SERVICE_NAME: &str = "loki-services";
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub service: &'static str,
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_seconds: u64,
}

impl HealthResponse {
    pub fn from_runtime(runtime: &RuntimeState) -> Self {
        Self {
            service: SERVICE_NAME,
            status: "ok",
            version: SERVICE_VERSION,
            uptime_seconds: runtime.uptime().as_secs(),
        }
    }
}
