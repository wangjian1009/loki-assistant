use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc};

use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::Value;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::modules::emacs::service::EmacsService;

use super::{
    mailbox::{ConversationRow, UserProfile},
    picker::PickerItem,
    protocol::{Error, Page},
    service::SlackService,
};

type Work = Pin<Box<dyn Future<Output = ()> + Send>>;
type Request = Box<dyn FnOnce(Arc<SlackService>) -> Work + Send>;
const REQUEST_CAPACITY: usize = 64;

pub(super) struct Team {
    requests: mpsc::Sender<Request>,
    #[cfg(test)]
    pub(super) state: crate::process::Process<super::service::State>,
}

impl Team {
    pub fn start(
        service: Arc<SlackService>,
        emacs: Arc<EmacsService>,
    ) -> (Arc<Self>, JoinHandle<()>) {
        let (requests, mut inbox) = mpsc::channel::<Request>(REQUEST_CAPACITY);
        let team = Arc::new(Self {
            requests,
            #[cfg(test)]
            state: service.state.clone(),
        });
        let worker = tokio::spawn(async move {
            let mut active = FuturesUnordered::new();
            let background = service.clone().run(emacs);
            tokio::pin!(background);
            loop {
                tokio::select! {
                    _ = &mut background => break,
                    request = inbox.recv(), if active.len() < REQUEST_CAPACITY => {
                        let Some(request) = request else { break; };
                        active.push(request(service.clone()));
                    }
                    _ = active.next(), if !active.is_empty() => {}
                }
            }
        });
        (team, worker)
    }

    pub(super) async fn request<T: Send + 'static>(
        &self,
        operation: impl FnOnce(Arc<SlackService>) -> WorkResult<T> + Send + 'static,
    ) -> Result<T, Error> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Box::new(move |service| {
                Box::pin(async move {
                    let _ = reply.send(operation(service).await);
                })
            }))
            .await
            .map_err(|_| Error::Project)?;
        response.await.map_err(|_| Error::Project)?
    }

    pub async fn session_id(&self) -> Option<Uuid> {
        self.request(|service| Box::pin(async move { Ok(service.session_id().await) }))
            .await
            .ok()
            .flatten()
    }

    pub async fn needs_reauthorization(&self) -> bool {
        self.request(|service| Box::pin(async move { Ok(service.needs_reauthorization().await) }))
            .await
            .unwrap_or_default()
    }

    pub async fn subscribe_messages(
        &self,
        session: Uuid,
        id: String,
        subscription: Uuid,
    ) -> Result<bool, Error> {
        self.request(move |service| {
            Box::pin(async move { service.subscribe_messages(session, id, subscription).await })
        })
        .await
    }

    pub async fn unsubscribe_messages(&self, id: String, subscription: Uuid) {
        let _ = self
            .request(move |service| {
                Box::pin(async move {
                    service.unsubscribe_messages(id, subscription).await;
                    Ok(())
                })
            })
            .await;
    }

    pub async fn disconnect(&self, session: Option<Uuid>) -> Result<(), Error> {
        self.request(move |service| Box::pin(async move { service.disconnect(session).await }))
            .await
    }

    pub async fn open_dm(&self, session: Uuid, user: &str) -> Result<Value, Error> {
        let user = user.to_owned();
        self.request(move |service| Box::pin(async move { service.open_dm(session, &user).await }))
            .await
    }

    pub async fn open_target(
        &self,
        session: Uuid,
        id: &str,
        thread: Option<&str>,
        message: Option<&str>,
    ) -> Result<Value, Error> {
        let (id, thread, message) = (
            id.to_owned(),
            thread.map(str::to_owned),
            message.map(str::to_owned),
        );
        self.request(move |service| {
            Box::pin(async move {
                service
                    .open_target(session, &id, thread.as_deref(), message.as_deref())
                    .await
            })
        })
        .await
    }

    pub async fn messages(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        page: Page,
    ) -> Result<Value, Error> {
        self.request(move |service| {
            Box::pin(async move { service.messages(session, id, thread, page).await })
        })
        .await
    }

    pub async fn refresh_subscribed_threads(&self, session: Uuid, id: String, roots: Vec<String>) {
        if roots.is_empty() {
            return;
        }
        let _ = self
            .requests
            .send(Box::new(move |service| {
                Box::pin(async move {
                    if service.has_message_subscribers(&id).await {
                        service.refresh_threads(session, &id, roots).await;
                    }
                })
            }))
            .await;
    }

    pub async fn avatar(&self, session: Uuid, user: &str) -> Result<PathBuf, Error> {
        let user = user.to_owned();
        self.request(move |service| Box::pin(async move { service.avatar(session, &user).await }))
            .await
    }

    pub async fn attachment(
        &self,
        session: Uuid,
        id: &str,
        ts: &str,
        index: usize,
        preview: bool,
        local_path: bool,
    ) -> Result<(String, Vec<u8>, Option<PathBuf>), Error> {
        let (id, ts) = (id.to_owned(), ts.to_owned());
        self.request(move |service| {
            Box::pin(async move {
                service
                    .attachment(session, &id, &ts, index, preview, local_path)
                    .await
            })
        })
        .await
    }

    pub async fn mark_read(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        ts: String,
    ) -> Result<Value, Error> {
        self.request(move |service| {
            Box::pin(async move { service.mark_read(session, id, thread, ts).await })
        })
        .await
    }

    pub async fn enqueue(
        &self,
        session: Uuid,
        id: String,
        thread: Option<String>,
        key: Uuid,
        text: String,
    ) -> Result<(bool, Value), Error> {
        self.request(move |service| {
            Box::pin(async move { service.enqueue(session, id, thread, key, text).await })
        })
        .await
    }

    pub async fn send_record(&self, session: Uuid, id: Uuid) -> Result<Value, Error> {
        self.request(move |service| Box::pin(async move { service.send_record(session, id).await }))
            .await
    }

    pub async fn users(&self, session: Uuid) -> Result<Arc<[UserProfile]>, Error> {
        self.request(move |service| Box::pin(async move { service.users(session).await }))
            .await
    }

    pub async fn lookup_user(&self, session: Uuid, id: &str) -> Result<UserProfile, Error> {
        let id = id.to_owned();
        self.request(move |service| {
            Box::pin(async move { service.lookup_user(session, &id).await })
        })
        .await
    }

    pub async fn user_result(&self, session: Uuid, user: &UserProfile) -> Result<Value, Error> {
        let user = user.clone();
        self.request(move |service| {
            Box::pin(async move { service.user_result(session, &user).await })
        })
        .await
    }

    pub async fn conversation_rows(
        &self,
        session: Uuid,
        unread: bool,
        kind: &str,
    ) -> Result<(Vec<ConversationRow>, bool), Error> {
        let kind = kind.to_owned();
        self.request(move |service| {
            Box::pin(async move { service.conversation_rows(session, unread, &kind).await })
        })
        .await
    }

    pub async fn picker_workspace(
        &self,
        id: &str,
        name: &str,
    ) -> Option<(Value, Arc<[PickerItem]>)> {
        let (id, name) = (id.to_owned(), name.to_owned());
        self.request(move |service| {
            Box::pin(async move { Ok(service.picker_workspace(&id, &name).await) })
        })
        .await
        .ok()
    }

    pub async fn set_watch(
        &self,
        id: String,
        conversation: String,
        watching: bool,
    ) -> Result<(), Error> {
        self.request(move |service| {
            Box::pin(async move { service.set_watch(id, &conversation, watching).await })
        })
        .await
    }

    pub async fn credentials_for_reset(&self) -> Result<[Option<String>; 2], Error> {
        self.request(|service| Box::pin(async move { service.credentials_for_reset().await }))
            .await
    }

    pub async fn check_session(&self, session: Uuid) -> Result<(), Error> {
        self.request(move |service| Box::pin(async move { service.check_session(session).await }))
            .await
    }

    #[cfg(test)]
    pub async fn sync_reauthorization_alert(&self) {
        let _ = self
            .request(|service| {
                Box::pin(async move {
                    service.sync_reauthorization_alert().await;
                    Ok(())
                })
            })
            .await;
    }

    #[cfg(test)]
    pub async fn update_cache<T: Send + 'static>(
        &self,
        session: Uuid,
        operation: impl FnOnce(&mut super::store::Cache, &super::store::Account) -> Result<T, Error>
        + Send
        + 'static,
    ) -> Result<T, Error> {
        self.request(move |service| {
            Box::pin(async move { service.update_cache(session, operation).await })
        })
        .await
    }
}

type WorkResult<T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send>>;
