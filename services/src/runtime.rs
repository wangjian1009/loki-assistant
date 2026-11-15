use std::{
    env, fs, io,
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::services::Services;

const HTTP_ADDR_ENV: &str = "LOKI_ASSISTANT_HTTP_ADDR";
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:47321";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServicePfile {
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub version: String,
    pub started_at: u64,
}

impl ServicePfile {
    pub fn path() -> Result<PathBuf, RuntimeConfigError> {
        let home = env::var_os("HOME").ok_or(RuntimeConfigError::HomeUnavailable)?;
        Ok(PathBuf::from(home).join(".assistant/service"))
    }

    pub fn for_listener(address: SocketAddr) -> Self {
        Self {
            host: address.ip().to_string(),
            port: address.port(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    pub fn write(&self) -> Result<PathBuf, RuntimeConfigError> {
        let path = Self::path()?;
        let parent = path.parent().ok_or(RuntimeConfigError::PfilePathInvalid)?;
        fs::create_dir_all(parent).map_err(|source| RuntimeConfigError::Io {
            operation: "创建 services 信息目录",
            source,
        })?;
        let temporary = path.with_extension(format!("tmp.{}", self.pid));
        let data = serde_json::to_vec_pretty(self).map_err(|source| RuntimeConfigError::Json {
            operation: "生成 services 信息",
            source,
        })?;
        fs::write(&temporary, data).map_err(|source| RuntimeConfigError::Io {
            operation: "写入 services 信息",
            source,
        })?;
        fs::rename(&temporary, &path).map_err(|source| RuntimeConfigError::Io {
            operation: "更新 services 信息",
            source,
        })?;
        Ok(path)
    }

    pub fn remove_if_owner(&self) -> Result<(), RuntimeConfigError> {
        let path = Self::path()?;
        let Ok(data) = fs::read(&path) else {
            return Ok(());
        };
        let Ok(current) = serde_json::from_slice::<ServicePfile>(&data) else {
            return Ok(());
        };
        if current.pid == self.pid {
            fs::remove_file(path).map_err(|source| RuntimeConfigError::Io {
                operation: "删除 services 信息",
                source,
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeConfigError {
    #[error("环境变量 {name} 不是有效的 socket 地址：{value}")]
    InvalidAddress { name: &'static str, value: String },
    #[error("无法确定用户主目录")]
    HomeUnavailable,
    #[error("services 信息路径无效")]
    PfilePathInvalid,
    #[error("{operation}失败：{source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{operation}失败：{source}")]
    Json {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// 服务进程启动所需的最低配置，不代表配置中心的业务配置。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    pub http_addr: SocketAddr,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            http_addr: DEFAULT_HTTP_ADDR
                .parse()
                .expect("默认 HTTP 地址必须是有效的 loopback 地址"),
        }
    }
}

impl RuntimeConfig {
    pub fn from_env() -> Result<Self, RuntimeConfigError> {
        let Some(value) = env::var_os(HTTP_ADDR_ENV) else {
            return Ok(Self::default());
        };
        let value = value.to_string_lossy().into_owned();
        let http_addr = Self::parse_address(&value)?;
        Ok(Self { http_addr })
    }

    fn parse_address(value: &str) -> Result<SocketAddr, RuntimeConfigError> {
        let http_addr =
            SocketAddr::from_str(value).map_err(|_| RuntimeConfigError::InvalidAddress {
                name: HTTP_ADDR_ENV,
                value: value.to_owned(),
            })?;
        if !http_addr.ip().is_loopback() {
            return Err(RuntimeConfigError::InvalidAddress {
                name: HTTP_ADDR_ENV,
                value: value.to_owned(),
            });
        }
        Ok(http_addr)
    }
}

#[derive(Debug)]
pub struct RuntimeState {
    started_at: Instant,
    services: std::sync::Arc<Services>,
}

impl RuntimeState {
    pub fn new(services: std::sync::Arc<Services>) -> Self {
        Self {
            started_at: Instant::now(),
            services,
        }
    }

    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn services(&self) -> std::sync::Arc<Services> {
        std::sync::Arc::clone(&self.services)
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new(std::sync::Arc::new(Services::build()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_loopback() {
        let config = RuntimeConfig::default();
        assert_eq!(config.http_addr.to_string(), DEFAULT_HTTP_ADDR);
    }

    #[test]
    fn rejects_non_loopback_address() {
        assert!(RuntimeConfig::parse_address("192.0.2.1:47321").is_err());
    }

    #[test]
    fn rejects_invalid_address() {
        assert!(RuntimeConfig::parse_address("not-an-address").is_err());
    }
}
