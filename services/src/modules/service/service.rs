use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::modules::status::StatusService;

/// 发布本机名称、设备类型和本次进程启动时间；名称仅用于展示。
#[derive(Debug)]
pub struct SelfService {
    status: Arc<StatusService>,
    started_at: u64,
}

impl SelfService {
    pub fn new(status: Arc<StatusService>) -> Self {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let service = Self { status, started_at };
        let mut hostname = [0u8; 256];
        let name =
            if unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } == 0 {
                String::from_utf8_lossy(
                    &hostname[..hostname
                        .iter()
                        .position(|b| *b == 0)
                        .unwrap_or(hostname.len())],
                )
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect::<String>()
            } else {
                String::new()
            };
        let mut name = name;
        while name.len() > 128 {
            name.pop();
        }
        let name = if name.trim().is_empty() {
            "Loki 电脑".to_owned()
        } else {
            name
        };
        service.status.post("service", "name", json!(name));
        service
            .status
            .post("service", "device_type", json!("computer"));
        service
            .status
            .post("service", "start_at", json!(service.started_at));
        service
    }

    pub fn started_at(&self) -> u64 {
        self.started_at
    }
}
