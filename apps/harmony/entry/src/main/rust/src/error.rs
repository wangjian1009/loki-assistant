use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    NotSent,
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotConfiguredReason {
    ServiceNotSet,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotBoundReason {
    BindingRequired,
    BindingRevoked,
    InvalidCredential,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkReason {
    ConnectFailed,
    ConnectTimeout,
    SendFailed,
    ReceiveFailed,
    RequestTimeout,
    InvalidResponse,
    ResponseTooLarge,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelledReason {
    UserRequested,
    ServiceChanged,
    NetworkClosed,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServicesReason {
    Rejected,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvalidRequestReason {
    InvalidMethod,
    InvalidBody,
    InvalidQuery,
    InvalidPath,
    InvalidTimeout,
    RequestTooLarge,
    InvalidService,
    InvalidInvitation,
    InvalidDeviceName,
    ExpiredInvitation,
    InvitationTargetMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure<R> {
    pub reason: R,
    pub message: String,
    pub request_id: Option<String>,
    pub outcome: Outcome,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServicesFailure {
    pub reason: ServicesReason,
    pub message: String,
    pub request_id: Option<String>,
    pub outcome: Outcome,
    pub status: u16,
    pub source: String,
    pub code: String,
}

/// 各类失败分别定义原因，通过带 kind 标签的 Union 返回给调用者。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponsError {
    NotConfigured(Failure<NotConfiguredReason>),
    NotBound(Failure<NotBoundReason>),
    Network(Failure<NetworkReason>),
    Cancelled(Failure<CancelledReason>),
    Services(ServicesFailure),
    InvalidRequest(Failure<InvalidRequestReason>),
}

macro_rules! failures {
    ($reason:ident, $variant:ident, {$($case:ident => $message:literal),+ $(,)?}) => {
        impl From<$reason> for ResponsError {
            fn from(reason: $reason) -> Self {
                let message = match reason { $($reason::$case => $message),+ };
                Self::$variant(Failure {
                    reason, message: message.into(), request_id: None, outcome: Outcome::NotSent,
                })
            }
        }
    };
}
failures!(NotConfiguredReason, NotConfigured, {
    ServiceNotSet => "尚未设置电脑服务",
});
failures!(NotBoundReason, NotBound, {
    BindingRequired => "尚未绑定当前电脑，请先扫码绑定",
    BindingRevoked => "此手机尚未绑定或已被电脑解除绑定，请重新扫码",
    InvalidCredential => "绑定凭据无效或已失效，请重新扫码绑定",
});
failures!(NetworkReason, Network, {
    ConnectFailed => "暂时无法连接电脑，请确认电脑服务和网络已开启",
    ConnectTimeout => "连接电脑超时，请检查电脑服务和网络",
    SendFailed => "请求发送失败，请检查网络连接",
    ReceiveFailed => "响应读取失败，请检查网络连接",
    RequestTimeout => "请求超时，请检查电脑服务和网络",
    InvalidResponse => "电脑服务协议不兼容或响应无效，请更新电脑服务",
    ResponseTooLarge => "电脑响应超过大小限制",
});
failures!(CancelledReason, Cancelled, {
    UserRequested => "请求已取消",
    ServiceChanged => "电脑服务已切换，请求已取消",
    NetworkClosed => "网络模块已关闭，请求已取消",
});
failures!(InvalidRequestReason, InvalidRequest, {
    InvalidMethod => "请求方法无效",
    InvalidBody => "请求体不是有效 JSON",
    InvalidQuery => "查询参数格式无效",
    InvalidPath => "请求路径无效",
    InvalidTimeout => "请求超时必须为 1 至 30 秒",
    RequestTooLarge => "请求超过大小限制",
    InvalidService => "电脑服务身份或中继配置无效",
    InvalidInvitation => "这不是有效的 Loki 设备绑定二维码",
    InvalidDeviceName => "设备名称须为不超过 128 字节的非空文本，不能包含控制字符",
    ExpiredInvitation => "二维码已过期，请在电脑上重新生成",
    InvitationTargetMismatch => "二维码与当前电脑服务不匹配",
});

impl ResponsError {
    pub(crate) fn api(value: &Value, status: u16) -> Result<Self, Self> {
        let invalid = || Self::from(NetworkReason::InvalidResponse);
        let error = &value["error"];
        let source = error["source"]
            .as_str()
            .filter(|s| matches!(*s, "services" | "emacs" | "business"))
            .ok_or_else(invalid)?;
        let code = error["code"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(invalid)?;
        let message = error["message"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(invalid)?;
        if value["api_version"] != "1" || value["status"].as_u64() != Some(status as u64) {
            return Err(invalid());
        }
        if status == 403 && code == "peer-not-bound" {
            let mut failure = Self::from(NotBoundReason::BindingRevoked);
            failure.set_context(None, Outcome::Rejected);
            return Ok(failure);
        }
        if status == 403 && code == "invalid-binding-token" {
            let mut failure = Self::from(NotBoundReason::InvalidCredential);
            failure.set_context(None, Outcome::Rejected);
            return Ok(failure);
        }
        Ok(Self::Services(ServicesFailure {
            reason: ServicesReason::Rejected,
            message: message.into(),
            request_id: None,
            outcome: Outcome::Rejected,
            status,
            source: source.into(),
            code: code.into(),
        }))
    }
    pub fn message(&self) -> &str {
        match self {
            Self::NotConfigured(e) => &e.message,
            Self::NotBound(e) => &e.message,
            Self::Network(e) => &e.message,
            Self::Cancelled(e) => &e.message,
            Self::Services(e) => &e.message,
            Self::InvalidRequest(e) => &e.message,
        }
    }
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::NotConfigured(e) => e.outcome,
            Self::NotBound(e) => e.outcome,
            Self::Network(e) => e.outcome,
            Self::Cancelled(e) => e.outcome,
            Self::Services(e) => e.outcome,
            Self::InvalidRequest(e) => e.outcome,
        }
    }
    pub(crate) fn set_context(&mut self, id: Option<String>, outcome: Outcome) {
        macro_rules! update {
            ($e:expr) => {{
                $e.request_id = id;
                $e.outcome = outcome;
            }};
        }
        match self {
            Self::NotConfigured(e) => update!(e),
            Self::NotBound(e) => update!(e),
            Self::Network(e) => update!(e),
            Self::Cancelled(e) => update!(e),
            Self::Services(e) => update!(e),
            Self::InvalidRequest(e) => update!(e),
        }
    }
}
impl std::fmt::Display for ResponsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for ResponsError {}
