use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::modules::status::StatusService;

/// 服务模块只发布本次 services 进程的启动时间；状态保存在共享状态中心内存中。
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
        service
            .status
            .publish("service", "start_at", json!(service.started_at))
            .expect("服务状态路径固定且有效");
        service
    }

    pub fn started_at(&self) -> u64 {
        self.started_at
    }
}
