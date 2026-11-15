//! Loki Assistant 控制中心服务层。
//!
//! 目前只提供可运行的本机 HTTP 骨架。Emacs、语音和 macOS 平台模块在后续阶段按
//! `docs/control-center-services.org` 的边界逐步加入。

pub mod http;
pub mod modules;
pub mod runtime;
