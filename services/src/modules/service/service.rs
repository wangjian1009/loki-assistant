use std::net::IpAddr;
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
        let name = system_computer_name()
            .and_then(|name| display_name(&name))
            .or_else(|| display_name(&hostname()))
            .unwrap_or_else(|| "Loki 电脑".to_owned());
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

#[cfg(target_os = "macos")]
fn system_computer_name() -> Option<String> {
    let output = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(target_os = "macos"))]
fn system_computer_name() -> Option<String> {
    None
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let end = buf.iter().position(|byte| *byte == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn display_name(value: &str) -> Option<String> {
    let mut name: String = value.trim().chars().filter(|c| !c.is_control()).collect();
    let address = name
        .trim_matches(['[', ']'])
        .split('%')
        .next()
        .unwrap_or_default();
    if address.parse::<IpAddr>().is_ok() {
        return None;
    }
    while name.len() > 128 {
        name.pop();
    }
    (!name.is_empty()).then_some(name)
}
