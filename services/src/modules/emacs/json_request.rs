//! 通过临时文件向 Emacs 业务模块传递 JSON 请求。

use std::{io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Serialize, de::DeserializeOwned};

use super::service::{EmacsError, EmacsService};

#[derive(Clone, Copy)]
pub(crate) enum Endpoint {
    Doc,
    Xbloom,
}

pub(crate) enum RequestError {
    Gateway(EmacsError),
    Unavailable,
    Transport,
}

pub(crate) async fn call<T, R>(
    emacs: Arc<EmacsService>,
    endpoint: Endpoint,
    request: R,
) -> Result<T, RequestError>
where
    T: DeserializeOwned + Send + 'static,
    R: Serialize + Send + 'static,
{
    // 请求断开后，已入队的 Emacs 调用仍持有传输文件。
    tokio::spawn(async move {
        let file = tokio::task::spawn_blocking(move || PayloadFile::write(request))
            .await
            .map_err(|_| RequestError::Transport)??;
        let path = file.0.to_str().ok_or(RequestError::Transport)?;
        let argument = STANDARD.encode(path);
        let feature = match endpoint {
            Endpoint::Doc => "loki-assistant-doc",
            Endpoint::Xbloom => "loki-assistant-xbloom-service",
        };
        let script = format!(
            "(when (require '{feature} nil t) \
               ({feature}-request \
                 (decode-coding-string (base64-decode-string \"{argument}\") 'utf-8)))"
        );
        let reply = emacs
            .eval_running(&script)
            .await
            .map_err(RequestError::Gateway)?
            .ok_or_else(|| {
                RequestError::Gateway(EmacsError::ConnectionFailed(
                    "请启动已配置的 Emacs server".into(),
                ))
            })?;
        if reply == "nil" {
            return Err(RequestError::Unavailable);
        }
        let reply: String = serde_json::from_str(&reply).map_err(|_| invalid_reply())?;
        let bytes = STANDARD.decode(reply).map_err(|_| invalid_reply())?;
        serde_json::from_slice(&bytes).map_err(|_| invalid_reply())
    })
    .await
    .map_err(|_| RequestError::Transport)?
}

fn invalid_reply() -> RequestError {
    RequestError::Gateway(EmacsError::InvalidResponse(
        "Emacs JSON 响应格式无效".into(),
    ))
}

struct PayloadFile(PathBuf);

impl PayloadFile {
    fn write(request: impl Serialize) -> Result<Self, RequestError> {
        let path = std::env::temp_dir().join(format!("loki-emacs-{}.json", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| RequestError::Transport)?;
        let payload = Self(path);
        serde_json::to_writer(&mut file, &request).map_err(|_| RequestError::Transport)?;
        file.flush().map_err(|_| RequestError::Transport)?;
        Ok(payload)
    }
}

impl Drop for PayloadFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
