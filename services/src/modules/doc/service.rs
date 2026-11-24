use std::{io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::de::DeserializeOwned;

use super::protocol::{Failure, Reply, Request};
use crate::modules::emacs::service::{EmacsError, EmacsService};

#[derive(Debug, Clone)]
pub struct DocService {
    emacs: Arc<EmacsService>,
}

pub(super) enum DocError {
    Gateway(EmacsError),
    Business(Failure),
    Transport,
}

impl DocService {
    pub fn new(emacs: Arc<EmacsService>) -> Self {
        Self { emacs }
    }

    pub(super) async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        request: Request,
    ) -> Result<T, DocError> {
        let emacs = self.emacs.clone();
        // 请求断开后，已入队的 Emacs 调用仍持有传输文件。
        tokio::spawn(async move {
            let file = tokio::task::spawn_blocking(move || PayloadFile::write(request))
                .await
                .map_err(|_| DocError::Transport)??;
            let path = file.0.to_str().ok_or(DocError::Transport)?;
            let argument = STANDARD.encode(path);
            let script = format!(
                "(when (require 'loki-assistant-doc nil t) \
                   (loki-assistant-doc-request \
                     (decode-coding-string (base64-decode-string \"{argument}\") 'utf-8)))"
            );
            let reply = emacs
                .eval_running(&script)
                .await
                .map_err(DocError::Gateway)?
                .ok_or_else(|| {
                    DocError::Gateway(EmacsError::ConnectionFailed(
                        "请启动已配置的 Emacs server".into(),
                    ))
                })?;
            if reply == "nil" {
                return Err(DocError::Business(Failure::DocUnavailable));
            }
            let reply: String = serde_json::from_str(&reply).map_err(|_| invalid_reply())?;
            let bytes = STANDARD.decode(reply).map_err(|_| invalid_reply())?;
            match serde_json::from_slice(&bytes).map_err(|_| invalid_reply())? {
                Reply::Success { result } => Ok(result),
                Reply::Failure { error } => Err(DocError::Business(error)),
            }
        })
        .await
        .map_err(|_| DocError::Transport)?
    }
}

pub(super) fn invalid_reply() -> DocError {
    DocError::Gateway(EmacsError::InvalidResponse("文档响应格式无效".into()))
}

struct PayloadFile(PathBuf);

impl PayloadFile {
    fn write(request: Request) -> Result<Self, DocError> {
        let path = std::env::temp_dir().join(format!("loki-doc-{}.json", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_| DocError::Transport)?;
        let payload = Self(path);
        serde_json::to_writer(&mut file, &request).map_err(|_| DocError::Transport)?;
        file.flush().map_err(|_| DocError::Transport)?;
        Ok(payload)
    }
}

impl Drop for PayloadFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
