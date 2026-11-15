//! Loki Assistant 控制中心服务层。
//!
//! 目前提供可运行的本机 HTTP 骨架、服务发现模块和 macOS LaunchAgent 管理器。Emacs
//! 与语音模块在后续阶段按 `docs/control-center-services.org` 的边界逐步加入。

extern crate self as loki_services;

pub mod http;
pub mod modules;
pub mod platform;
pub mod runtime;
pub mod services;

pub use services::{CoreService, Services};
