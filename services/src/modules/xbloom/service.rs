use std::sync::Arc;

use serde::de::DeserializeOwned;

use super::protocol::{Failure, Reply, Request};
use crate::modules::emacs::{
    json_request::{self, Endpoint, RequestError},
    service::EmacsService,
};

#[derive(Debug, Clone)]
pub struct XbloomService {
    emacs: Arc<EmacsService>,
}

pub(super) enum Error {
    Request(RequestError),
    Business(Failure),
}

impl XbloomService {
    pub fn new(emacs: Arc<EmacsService>) -> Self {
        Self { emacs }
    }

    pub(super) async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        request: Request,
    ) -> Result<T, Error> {
        match json_request::call(self.emacs.clone(), Endpoint::Xbloom, request).await {
            Ok(Reply::Success { result }) => Ok(result),
            Ok(Reply::Failure { error }) => Err(Error::Business(error)),
            Err(RequestError::Unavailable) => Err(Error::Business(Failure::XbloomUnavailable)),
            Err(error) => Err(Error::Request(error)),
        }
    }
}
