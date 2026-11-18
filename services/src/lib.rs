//! Loki Assistant 控制中心服务层。
//!
//! 提供本机 HTTP 服务、服务发现、配置中心、状态中心、系统通知、Emacs 模块和 macOS LaunchAgent 管理器。

extern crate self as loki_services;

pub mod http;
pub mod modules;
pub mod platform;
pub mod runtime;
pub mod services;

pub use services::{CoreService, Services};
