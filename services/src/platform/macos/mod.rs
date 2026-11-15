//! macOS launchd 服务管理。

mod launchd;
pub mod notification;

pub use launchd::{
    CommandOutput, CommandRunner, LaunchdError, LaunchdManager, LaunchdServiceSpec, LaunchdStatus,
};
