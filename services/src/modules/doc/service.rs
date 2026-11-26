use std::sync::Arc;

use serde::de::DeserializeOwned;

use super::protocol::{Failure, Reply, Request};
use crate::modules::emacs::{
    json_request::{self, Endpoint, RequestError},
    service::{EmacsError, EmacsService},
};

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
        match json_request::call(self.emacs.clone(), Endpoint::Doc, request).await {
            Ok(Reply::Success { result }) => Ok(result),
            Ok(Reply::Failure { error }) => Err(DocError::Business(error)),
            Err(RequestError::Gateway(error)) => Err(DocError::Gateway(error)),
            Err(RequestError::Unavailable) => Err(DocError::Business(Failure::DocUnavailable)),
            Err(RequestError::Transport) => Err(DocError::Transport),
        }
    }
}

pub(super) fn invalid_reply() -> DocError {
    DocError::Gateway(EmacsError::InvalidResponse("文档响应格式无效".into()))
}
