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
            StatusItemMetadata {
                key: "name".to_owned(),
                name: "设备名称".to_owned(),
                description: "由本机 hostname 生成的只读显示名称，不用于认证。".to_owned(),
                schema: StatusSchema::string(),
            },
            StatusItemMetadata {
                key: "device_type".to_owned(),
                name: "设备类型".to_owned(),
                description: "当前 services 为 computer，移动端为 mobile。".to_owned(),
                schema: StatusSchema::string(),
            },
            StatusItemMetadata {
                key: "start_at".to_owned(),
                name: "启动时间".to_owned(),
                description: "本次 services 进程启动时间，Unix 秒。".to_owned(),
                schema: StatusSchema::Integer,
            },
        ],
    }
}
