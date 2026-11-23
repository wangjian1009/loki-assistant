use std::{
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, Semaphore, watch};
use uuid::Uuid;

use crate::modules::{
    config::service::ConfigService,
    notification::{NotificationService, protocol::NotificationRequest},
};

use super::{
    client::{SlackClient, WebSession},
    disk::DiskCache,
    mailbox::UserDirectory,
    picker::PickerItem,
    projects::{Backend, SlackTeam, read_watches},
    protocol::{Error, Identity, valid_id},
    status::ProjectsStatus,
    store::{Account, Cache, Nicknames, Secrets},
};

#[derive(Default)]
pub(super) struct State {
    pub user_directory: Option<UserDirectory>,
    pub picker_items: Arc<[PickerItem]>,
    pub picker_failed: bool,
    pub secrets: Secrets,
    pub cache: Cache,
    pub socket_state: &'static str,
    pub issue: Option<Error>,
    rejected_credentials: Option<String>,
    pub(super) retired: bool,
}

impl State {
    pub fn account(&self, session: Uuid) -> Result<&Account, Error> {
        if self.retired {
            return Err(Error::Project);
        }
        let account = self.secrets.account.as_ref().ok_or(Error::Session)?;
        if account.identity.session_id != session {
            return Err(Error::Session);
        }
        if account.revoked || (account.access.is_empty() && !account.web_session) {
            return Err(Error::Authorization);
        }
        Ok(account)
    }
}

pub struct SlackService {
    pub(super) state: Arc<Mutex<State>>,
    cache_root: Option<PathBuf>,
    pub(super) client: SlackClient,
    pub(super) notification: Arc<NotificationService>,
    pub(super) change: watch::Sender<u64>,
    pub(super) outgoing: Notify,
    pub(super) sync: Notify,
    pub(super) alerts: Notify,
    pub(super) user_fetch: Mutex<()>,
    pub(super) directory_fetch: Mutex<()>,
    pub(super) picker_changed: Notify,
    pub(super) avatars_changed: Notify,
    pub(super) preview_downloads: Semaphore,
    pub(super) subscriptions_changed: Notify,
    pub(super) watch_changes: Arc<Mutex<()>>,
    project: (String, SlackTeam, Uuid, Arc<ProjectsStatus>),
    config: Arc<ConfigService>,
    imported_token_lock: Mutex<()>,
    nicknames: Nicknames,
}

impl fmt::Debug for SlackService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SlackService")
            .finish_non_exhaustive()
    }
}

impl SlackService {
    pub(super) fn for_project(
        notification: Arc<NotificationService>,
        id: String,
        settings: SlackTeam,
        generation: Uuid,
        reporter: Arc<ProjectsStatus>,
        backend: &Backend,
        config: Arc<ConfigService>,
        watch_changes: Arc<Mutex<()>>,
        nicknames: Nicknames,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                socket_state: "stopped",
                ..State::default()
            })),
            cache_root: backend.cache_root.clone(),
            client: SlackClient::new(&backend.api),
            notification,
            change: watch::channel(0).0,
            outgoing: Notify::new(),
            sync: Notify::new(),
            alerts: Notify::new(),
            user_fetch: Mutex::default(),
            directory_fetch: Mutex::default(),
            picker_changed: Notify::new(),
            avatars_changed: Notify::new(),
            preview_downloads: Semaphore::new(4),
            subscriptions_changed: Notify::new(),
            watch_changes,
            project: (id, settings, generation, reporter),
            config,
            imported_token_lock: Mutex::new(()),
            nicknames,
        }
    }

    pub(super) async fn watching(&self, id: &str) -> bool {
        read_watches(&self.config)
            .await
            .is_ok_and(|watches| watches.iter().any(|watch| watch.id == id))
    }

    pub(super) async fn watch_conversation_name(&self, id: &str) -> Result<String, Error> {
        let session = self.session_id().await.ok_or(Error::Session)?;
        if !self.state.lock().await.cache.conversations.contains_key(id) {
            self.refresh_directory(session).await?;
        }
        let state = self.state.lock().await;
        state.account(session)?;
        let room = state
            .cache
            .conversations
            .get(id)
            .ok_or(Error::Conversation)?;
        if !matches!(room.kind.as_str(), "public_channel" | "private_channel") {
            return Err(Error::WatchUnsupported);
        }
        Ok(format!("#{}", room.title))
    }

    pub(super) fn signal_change(&self) {
        self.change.send_modify(|generation| *generation += 1);
        self.alerts.notify_one();
    }

    pub(super) async fn retire(&self) {
        let mut state = self.state.lock().await;
        state.retired = true;
        state.cache.clear_subscriptions();
        self.signal_change();
    }

    pub(super) async fn nicknames_changed(&self) {
        let mut state = self.state.lock().await;
        let (id, _, generation, reporter) = &self.project;
        if let Some(account) = state
            .cache
            .session_id
            .and_then(|session| state.account(session).ok())
        {
            reporter.update_new_conversations(
                id,
                *generation,
                Some((&state.cache, account, state.user_directory.as_ref())),
            );
            for room in state.cache.conversations.values_mut() {
                if let Some(journal) = &mut room.journal {
                    journal.nicknames_changed();
                }
            }
        }
        self.picker_changed.notify_one();
    }

    pub(super) async fn forget_watch(&self, watch: &str) {
        self.state
            .lock()
            .await
            .cache
            .notifications
            .retain(|_, alert| alert.watch.as_deref() != Some(watch));
    }

    pub(super) async fn update_cache<T: Send + 'static>(
        &self,
        session: Uuid,
        operation: impl FnOnce(&mut Cache, &Account) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let mut state = self.state.clone().lock_owned().await;
        state.account(session)?;
        let (_, _, generation, reporter) = &self.project;
        let (generation, reporter) = (*generation, Arc::clone(reporter));
        let result = tokio::task::spawn_blocking(move || {
            let State {
                cache,
                secrets,
                user_directory,
                ..
            } = &mut *state;
            let account = secrets.account.as_ref().expect("已验证的 Slack 会话");
            let result = operation(cache, account);
            if result.is_ok() {
                reporter.update_new_conversations(
                    &account.identity.team_id,
                    generation,
                    Some((cache, account, user_directory.as_ref())),
                );
            }
            result
        })
        .await
        .map_err(|_| Error::Unavailable)?;
        if result.is_ok() {
            self.picker_changed.notify_one();
        }
        result
    }

    async fn update_secrets<T>(
        &self,
        operation: impl FnOnce(&mut Secrets) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut state = self.state.clone().lock_owned().await;
        if state.retired {
            return Err(Error::Project);
        }
        let result = operation(&mut state.secrets)?;
        let identity = state
            .secrets
            .account
            .as_ref()
            .map(|account| account.identity.clone());
        let session = identity.as_ref().map(|identity| identity.session_id);
        let authorization_lost = session.is_some_and(|session| state.account(session).is_err());
        if state.cache.session_id != session || authorization_lost {
            let (id, _, generation, reporter) = &self.project;
            reporter.update_new_conversations(id, *generation, None);
        }
        if state.cache.session_id != session {
            state.user_directory = None;
            state.picker_items = Arc::default();
            state.picker_failed = false;
            state.cache.reset(session);
            if let (Some(root), Some(identity)) = (self.cache_root.clone(), identity) {
                let (_, _, generation, reporter) = &self.project;
                let (generation, reporter) = (*generation, Arc::clone(reporter));
                tokio::task::spawn_blocking(move || {
                    if let Ok(disk) = DiskCache::open(&root, &identity.team_id, &identity.user_id) {
                        let account = state.secrets.account.as_mut().expect("已验证的 Slack 会话");
                        let mut rooms = disk.rooms();
                        if let Some(baseline) = rooms
                            .values()
                            .filter_map(|room| room.read_positions.baseline)
                            .min()
                        {
                            account.baseline = baseline;
                        }
                        for room in rooms.values_mut() {
                            room.read_positions.baseline.get_or_insert(account.baseline);
                        }
                        state.user_directory = Some(UserDirectory::from_disk(
                            identity.session_id,
                            disk.users(&identity.team_id),
                        ));
                        state.cache.conversations = rooms;
                        state.cache.disk = Some(disk);
                        if let Ok(account) = state.account(identity.session_id) {
                            reporter.update_new_conversations(
                                &identity.team_id,
                                generation,
                                Some((&state.cache, account, state.user_directory.as_ref())),
                            );
                        }
                    }
                })
                .await
                .map_err(|_| Error::Unavailable)?;
            }
        } else if authorization_lost {
            state.cache.clear_subscriptions();
        }
        self.picker_changed.notify_one();
        Ok(result)
    }

    fn authorization_state(state: &State) -> &'static str {
        let account = state.secrets.account.as_ref();
        match account {
            Some(account) if account.revoked => "reauthorization_required",
            Some(account) if !account.access.is_empty() || account.web_session => "authorized",
            _ if state.issue.is_some_and(Error::requires_reauthorization) => {
                "reauthorization_required"
            }
            _ => "unconfigured",
        }
    }

    pub(super) async fn session_id(&self) -> Option<Uuid> {
        let state = self.state.lock().await;
        let session = state.secrets.account.as_ref()?.identity.session_id;
        state.account(session).ok().map(|_| session)
    }

    pub(super) async fn needs_reauthorization(&self) -> bool {
        Self::authorization_state(&*self.state.lock().await) == "reauthorization_required"
    }

    pub(super) async fn public_status(&self) -> Value {
        let state = self.state.lock().await;
        let account = state.secrets.account.as_ref().filter(|account| {
            !account.revoked && (!account.access.is_empty() || account.web_session)
        });
        let (team_id, settings, _, _) = &self.project;
        json!({"team_id":team_id,"name":settings.name,
               "team_name":account.map(|account| &account.identity.team_name),
               "session_id":account.map(|account| account.identity.session_id),
               "user_id":account.map(|account| format!("{team_id}.{}", account.identity.user_id)),
               "authorization_state":Self::authorization_state(&state),
               "socket_state":state.socket_state,
               "issue_code":state.issue.map(|error| error.object()["code"].clone())})
    }

    pub(super) async fn publish_status(&self) {
        let (id, _, generation, reporter) = &self.project;
        reporter.update(id, *generation, self.public_status().await);
    }

    pub(super) async fn notify_reauthorization(&self) -> bool {
        let (id, settings, _, _) = &self.project;
        self.notification
            .send(NotificationRequest {
                title: "Slack 需要重新授权".into(),
                body: Some(format!("点击为「{}」重新填写 Slack 凭据。", settings.name)),
                app: Some("slack".into()),
                callback_url: Some(super::routes::credential_reset_path(id)),
            })
            .await
            .is_ok()
    }

    async fn verify_user_token(&self, token: &str) -> Result<Identity, Error> {
        if !token.starts_with("xoxp-")
            || !(6..=4096).contains(&token.len())
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::UserToken);
        }
        self.verify_identity(token).await
    }

    async fn verify_identity(&self, token: &str) -> Result<Identity, Error> {
        let identity = self.client.test_identity(token).await?;
        self.identity_from_response(identity)
    }

    fn identity_from_response(&self, identity: Value) -> Result<Identity, Error> {
        if identity["bot_id"].as_str().is_some() {
            return Err(Error::UserToken);
        }
        let team = identity["team_id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or(Error::Upstream)?
            .to_owned();
        if self
            .project
            .1
            .team_id
            .as_ref()
            .is_some_and(|expected| expected != &team)
        {
            return Err(Error::TeamMismatch);
        }
        let user = identity["user_id"]
            .as_str()
            .filter(|id| valid_id(id))
            .ok_or(Error::Upstream)?
            .to_owned();
        let name = identity["team"].as_str().unwrap_or(&team).to_owned();
        Ok(Identity {
            session_id: Uuid::new_v4(),
            team_id: team,
            user_id: user,
            team_name: name,
        })
    }

    pub(super) async fn restore_imported_token(&self) -> Result<(), Error> {
        if self.project.1.token.is_none() && self.project.1.d_cookie.is_none() {
            return Ok(());
        }
        let _lock = self.imported_token_lock.lock().await;
        let passwords = self.read_passwords().await?;
        let token = imported_token(&passwords)?;
        let hash = imported_token_hash(token, &passwords);
        let result = self.restore_credentials(token, &passwords, &hash).await;
        self.state.lock().await.rejected_credentials =
            (result == Err(Error::Authorization)).then_some(hash);
        result
    }

    async fn restore_credentials(
        &self,
        token: &str,
        passwords: &[Option<String>; 2],
        hash: &str,
    ) -> Result<(), Error> {
        let identity = {
            let state = self.state.lock().await;
            if state.secrets.disabled_token.as_deref() == Some(hash) {
                return Ok(());
            }
            let previous = state
                .secrets
                .account
                .as_ref()
                .filter(|account| account.imported_token_hash.as_deref() == Some(hash));
            if previous.is_some_and(|account| account.revoked) {
                return Err(Error::Authorization);
            }
            if previous.is_some_and(|account| !account.access.is_empty() || account.web_session) {
                return Ok(());
            }
            previous.map(|account| account.identity.clone())
        };
        let web_session = token.starts_with("xoxc-");
        let identity = match identity {
            Some(identity) => identity,
            None => self.verify_imported_credentials(token, passwords).await?,
        };
        let access = if web_session {
            String::new()
        } else {
            token.to_owned()
        };
        self.save_user(identity, access, hash.to_owned(), web_session)
            .await
    }

    async fn verify_imported_credentials(
        &self,
        token: &str,
        passwords: &[Option<String>; 2],
    ) -> Result<Identity, Error> {
        if token.starts_with("xoxc-") {
            let session =
                WebSession::from_passwords(passwords)?.ok_or(Error::WebSessionIncomplete)?;
            let response = self
                .client
                .web_call("auth.test", &session, &[], None)
                .await?;
            self.identity_from_response(response)
        } else {
            self.verify_user_token(token).await
        }
    }

    pub(super) async fn credentials_for_reset(&self) -> Result<[Option<String>; 2], Error> {
        let passwords = self.read_passwords().await?;
        let state = self.state.lock().await;
        let resettable = match imported_token(&passwords) {
            Ok(token) => {
                let hash = imported_token_hash(token, &passwords);
                state.rejected_credentials.as_deref() == Some(&hash)
                    || state.secrets.account.as_ref().is_some_and(|account| {
                        account.revoked && account.imported_token_hash.as_deref() == Some(&hash)
                    })
            }
            Err(_) => state.issue == Some(Error::WebSessionIncomplete),
        };
        if resettable {
            Ok(passwords)
        } else {
            Err(Error::CredentialsCurrent)
        }
    }

    async fn save_user(
        &self,
        identity: Identity,
        token: String,
        imported_token_hash: String,
        web_session: bool,
    ) -> Result<(), Error> {
        let mut account = Account {
            nicknames: Arc::clone(&self.nicknames),
            identity,
            access: token,
            web_session,
            baseline: now(),
            imported_token_hash: Some(imported_token_hash),
            revoked: false,
        };
        self.update_secrets(move |secrets| {
            if let Some(previous) = &secrets.account
                && previous.identity.team_id == account.identity.team_id
                && previous.identity.user_id == account.identity.user_id
            {
                account.identity.session_id = previous.identity.session_id;
                account.baseline = previous.baseline;
            }
            secrets.account = Some(account);
            secrets.disabled_token = None;
            Ok(())
        })
        .await?;
        self.state.lock().await.issue = None;
        self.signal_change();
        self.publish_status().await;
        Ok(())
    }

    pub(super) async fn disconnect(&self, session: Option<Uuid>) -> Result<(), Error> {
        self.update_secrets(move |secrets| {
            if let Some(session) = session
                && secrets
                    .account
                    .as_ref()
                    .is_some_and(|account| account.identity.session_id != session)
            {
                return Err(Error::Session);
            }
            if let Some(hash) = secrets
                .account
                .as_ref()
                .and_then(|account| account.imported_token_hash.clone())
            {
                secrets.disabled_token = Some(hash);
            }
            secrets.account = None;
            Ok(())
        })
        .await?;
        self.signal_change();
        self.publish_status().await;
        Ok(())
    }

    pub(super) async fn revoke(&self, session: Uuid, token: &str) {
        let token = token.to_owned();
        let revoked = self
            .update_secrets(move |secrets| {
                if let Some(account) = &mut secrets.account
                    && account.identity.session_id == session
                    && account.access == token
                {
                    account.revoked = true;
                    return Ok(true);
                }
                Ok(false)
            })
            .await;
        if revoked == Ok(true) {
            self.signal_change();
            self.publish_status().await;
        }
    }

    pub(super) async fn web_session(&self, session: Uuid) -> Result<Option<WebSession>, Error> {
        if !self.state.lock().await.account(session)?.web_session {
            return Ok(None);
        }
        let web = WebSession::from_passwords(&self.read_passwords().await?)?
            .ok_or(Error::WebSessionIncomplete)?;
        Ok(Some(web))
    }

    async fn read_passwords(&self) -> Result<[Option<String>; 2], Error> {
        let (team, _, _, _) = &self.project;
        self.config
            .read_record_passwords("slack", "teams", team, SlackTeam::PASSWORD_FIELDS)
            .await
            .map_err(|_| Error::PasswordRead)
    }

    pub(super) async fn slack_post(
        &self,
        session: Uuid,
        method: &str,
        body: &Value,
    ) -> Result<Value, Error> {
        if let Some(web) = self.web_session(session).await? {
            return self.client.web_call(method, &web, &[], Some(body)).await;
        }
        let token = self.state.lock().await.account(session)?.access.clone();
        self.client.post(method, &token, body).await
    }

    pub(super) async fn slack_get(
        &self,
        session: Uuid,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, Error> {
        if let Some(web) = self.web_session(session).await? {
            let result = self.client.web_call(method, &web, params, None).await;
            if result == Err(Error::Authorization) {
                self.revoke(session, "").await;
            }
            return result;
        }
        let token = self.state.lock().await.account(session)?.access.clone();
        let result = self.client.get(method, &token, params).await;
        if result == Err(Error::Authorization) {
            self.revoke(session, &token).await;
        }
        result
    }
}

fn imported_token(passwords: &[Option<String>; 2]) -> Result<&str, Error> {
    let token = passwords[0]
        .as_deref()
        .ok_or(Error::WebSessionIncomplete)?
        .trim_end_matches('\n');
    if token.starts_with("xoxc-") && passwords[1].is_none() {
        return Err(Error::WebSessionIncomplete);
    }
    Ok(token)
}

fn imported_token_hash(token: &str, passwords: &[Option<String>; 2]) -> String {
    let mut digest = Sha256::new();
    digest.update(token.as_bytes());
    if let Some(cookie) = &passwords[1] {
        digest.update([0]);
        digest.update(cookie.trim().as_bytes());
    }
    format!("{:x}", digest.finalize())
}

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
