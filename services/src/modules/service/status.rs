use crate::modules::status::model::{StatusItemMetadata, StatusModuleMetadata, StatusSchema};
use loki_services_macros::loki_status;

#[loki_status]
fn service_status_metadata() -> StatusModuleMetadata {
    StatusModuleMetadata {
        id: "service".to_owned(),
        name: "服务".to_owned(),
        description: "本机 services 进程的自身运行信息。".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        items: vec![
            StatusItemMetadata::new(
                "name",
                "设备名称",
                "由本机 hostname 生成的只读显示名称，不用于认证。",
                StatusSchema::string(),
            ),
            StatusItemMetadata::new(
                "device_type",
                "设备类型",
                "当前 services 为 computer，移动端为 mobile。",
                StatusSchema::string(),
            ),
            StatusItemMetadata::new(
                "start_at",
                "启动时间",
                "本次 services 进程启动时间，Unix 秒。",
                StatusSchema::Integer,
            ),
        ],
    }
}
