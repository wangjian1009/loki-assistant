use std::{
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, watch};
use uuid::Uuid;

use crate::modules::{config::service::ConfigService, notification::NotificationService};

use super::{
    client::{SlackClient, WebSession},
    mailbox::UserDirectory,
    projects::{Backend, SlackTeam},
    protocol::{Error, Identity, valid_id},
    status::ProjectsStatus,
    store::{Account, Cache, Keychain, Secrets, SendState},
};

#[derive(Default)]
pub(super) struct State {
    pub initialized: bool,
    pub credentials_locked: bool,
    pub storage_unavailable: bool,
    pub secrets: Secrets,
    pub cache: Cache,
    pub socket_state: &'static str,
    pub issue: Option<Error>,
    retired: bool,
}

impl State {
    pub fn account(&self, session: Uuid) -> Result<&Account, Error> {
        if self.retired {
            return Err(Error::Project);
        }
        if self.credentials_locked {
            return Err(Error::Credentials);
        }
        if self.storage_unavailable {
            return Err(Error::Storage);
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
    pub(super) client: SlackClient,
    pub(super) notification: Arc<NotificationService>,
    pub(super) change: watch::Sender<u64>,
    pub(super) outgoing: Notify,
    pub(super) sync: Notify,
    pub(super) alerts: Notify,
    pub(super) user_directory: Mutex<Option<UserDirectory>>,
    keychain: Arc<dyn Keychain>,
    cache_path: Option<PathBuf>,
    project: Option<(String, SlackTeam, Uuid, Arc<ProjectsStatus>)>,
    imported_token: Option<Arc<ConfigService>>,
    imported_token_lock: Mutex<()>,
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
    ) -> Self {
        // 沿用未配置 Client ID 时的工作区 Keychain 和缓存名称。
        let namespace = format!("{id}--{}", settings.team_id.as_deref().unwrap_or_default());
        let mut service = Self::with_dependencies(
            notification,
            (backend.keychain)(namespace.clone()),
            &backend.api,
            backend.path.clone(),
        );
        service.cache_path = service
            .cache_path
            .map(|path| path.with_file_name(format!("slack-cache-{namespace}.json")));
        if settings.token.is_some() {
            service.imported_token = Some(config);
        }
        service.project = Some((id, settings, generation, reporter));
        service
    }

    fn with_dependencies(
        notification: Arc<NotificationService>,
        keychain: Arc<dyn Keychain>,
        api: &str,
        service_path: Option<PathBuf>,
    ) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                socket_state: "stopped",
                ..State::default()
            })),
            client: SlackClient::new(api),
            notification,
            change: watch::channel(0).0,
            outgoing: Notify::new(),
            sync: Notify::new(),
            alerts: Notify::new(),
            user_directory: Mutex::default(),
            keychain,
            cache_path: service_path
                .as_ref()
                .map(|path| path.with_file_name("slack-cache.json")),
            project: None,
            imported_token: None,
            imported_token_lock: Mutex::new(()),
        }
    }

    pub(super) async fn initialize(&self) -> Result<(), Error> {
        let keychain = Arc::clone(&self.keychain);
        let cache_path = self.cache_path.clone();
        let has_imported_password = self
            .project
            .as_ref()
            .is_none_or(|(_, settings, _, _)| settings.token.is_some());
        let mut state = Arc::clone(&self.state).lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if state.initialized && !state.credentials_locked {
                return Ok(());
            }

            if !state.initialized {
                let cache = cache_path
                    .as_deref()
                    .ok_or(Error::Storage)
                    .and_then(Cache::load);
                match cache {
                    Ok(cache) => state.cache = cache,
                    Err(_) => state.storage_unavailable = true,
                }
                state.initialized = true;
            }
            match keychain.load() {
                Ok(mut secrets) => {
                    if !has_imported_password
                        || secrets
                            .account
                            .as_ref()
                            .is_some_and(|account| account.imported_token_hash.is_none())
                    {
                        secrets.account = None;
                    }
                    state.credentials_locked = false;
                    let session = secrets
                        .account
                        .as_ref()
                        .map(|account| account.identity.session_id);
                    state.cache.reset(session);
                    for record in state.cache.sends.values_mut() {
                        if record.state == SendState::Attempting {
                            record.state = SendState::Unknown;
                            record.failure = Some(Error::DeliveryUnknown);
                            record.text = None;
                        }
                    }
                    state.secrets = secrets;
                    if !state.storage_unavailable && state.cache.save(cache_path.as_ref()).is_err()
                    {
                        state.storage_unavailable = true;
                    }
                    Ok(())
                }
                Err(error) => {
                    state.credentials_locked = true;
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| Error::Storage)?
    }

    pub(super) fn signal_change(&self) {
        self.change.send_modify(|generation| *generation += 1);
        self.alerts.notify_one();
    }

    pub(super) async fn retire(&self) {
        let mut state = self.state.lock().await;
        state.retired = true;
        self.signal_change();
    }

    pub(super) async fn update_cache<T: Send + 'static>(
        &self,
        session: Uuid,
        operation: impl FnOnce(&mut Cache, &Account) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let mut state = Arc::clone(&self.state).lock_owned().await;
        let account = state.account(session)?.clone();
        let path = self.cache_path.clone();
        tokio::task::spawn_blocking(move || {
            let mut cache = state.cache.clone();
            let result = operation(&mut cache, &account)?;
            cache.save(path.as_ref())?;
            state.cache = cache;
            Ok(result)
        })
        .await
        .map_err(|_| Error::Storage)?
    }

    async fn update_secrets<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Secrets, &State) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let mut state = Arc::clone(&self.state).lock_owned().await;
        let keychain = Arc::clone(&self.keychain);
        let path = self.cache_path.clone();
        tokio::task::spawn_blocking(move || {
            if state.retired {
                return Err(Error::Project);
            }
            if state.credentials_locked {
                return Err(Error::Credentials);
            }
            if state.storage_unavailable {
                return Err(Error::Storage);
            }
            let mut next = state.secrets.clone();
            let result = operation(&mut next, &state)?;
            let mut stored = next.clone();
            if let Some(account) = &mut stored.account
                && account.imported_token_hash.is_some()
            {
                account.access.clear();
            }
            keychain.save(&stored)?;
            let session = next
                .account
                .as_ref()
                .map(|account| account.identity.session_id);
            state.secrets = next;
            if state.cache.session_id != session {
                state.cache.reset(session);
                if state.cache.save(path.as_ref()).is_err() {
                    state.storage_unavailable = true;
                    return Err(Error::Storage);
                }
            }
            Ok(result)
        })
        .await
        .map_err(|_| Error::Credentials)?
    }

    fn authorization_state(state: &State) -> &'static str {
        let account = state.secrets.account.as_ref();
        match account {
            _ if state.credentials_locked => "credentials_unavailable",
            Some(account) if account.revoked => "reauthorization_required",
            Some(account) if !account.access.is_empty() || account.web_session => "authorized",
            _ => "unconfigured",
        }
    }

    pub(super) async fn session_id(&self) -> Option<Uuid> {
        let state = self.state.lock().await;
        let session = state.secrets.account.as_ref()?.identity.session_id;
        state.account(session).ok().map(|_| session)
    }

    pub(super) async fn public_status(&self) -> Value {
        let state = self.state.lock().await;
        let account = state.secrets.account.as_ref().filter(|account| {
            !state.credentials_locked
                && !account.revoked
                && (!account.access.is_empty() || account.web_session)
        });
        let (team_id, settings, _, _) = self.project.as_ref().expect("Slack 工作区配置");
        let issue = if state.storage_unavailable {
            Some(Error::Storage)
        } else {
            state.issue
        };
        json!({"team_id":team_id,"name":settings.name,
               "team_name":account.map(|account| &account.identity.team_name),
               "session_id":account.map(|account| account.identity.session_id),
               "user_id":account.map(|account| format!("{team_id}.{}", account.identity.user_id)),
               "authorization_state":Self::authorization_state(&state),
               "socket_state":state.socket_state,
               "issue_code":issue.map(|error| error.object()["code"].clone())})
    }

    pub(super) async fn publish_status(&self) {
        if let Some((id, _, generation, reporter)) = &self.project {
            reporter.update(id, *generation, self.public_status().await);
        }
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
            .as_ref()
            .and_then(|(_, settings, _, _)| settings.team_id.as_ref())
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
        if self.imported_token.is_none() {
            return Ok(());
        }
        let _lock = self.imported_token_lock.lock().await;
        let passwords = self.read_passwords().await?;
        let token = passwords[0]
            .as_deref()
            .ok_or(Error::PasswordRead)?
            .trim_end_matches('\n');
        let mut digest = Sha256::new();
        digest.update(token.as_bytes());
        for secret in &passwords[1..] {
            digest.update([0]);
            if let Some(secret) = secret {
                digest.update(secret.trim().as_bytes());
            }
        }
        let hash = if passwords[1..].iter().all(Option::is_none) {
            format!("{:x}", Sha256::digest(token.as_bytes()))
        } else {
            format!("{:x}", digest.finalize())
        };
        let identity = {
            let state = self.state.lock().await;
            if state.secrets.disabled_token.as_deref() == Some(&hash) {
                return Ok(());
            }
            let previous = state
                .secrets
                .account
                .as_ref()
                .filter(|account| account.imported_token_hash.as_deref() == Some(&hash));
            if previous.is_some_and(|account| {
                account.revoked || !account.access.is_empty() || account.web_session
            }) {
                return Ok(());
            }
            previous.map(|account| account.identity.clone())
        };
        if token.starts_with("xoxc-") {
            let session =
                WebSession::from_passwords(&passwords)?.ok_or(Error::WebSessionIncomplete)?;
            let identity = match identity {
                Some(identity) => identity,
                None => {
                    let response = self
                        .client
                        .web_call("auth.test", &session, &[], None)
                        .await?;
                    self.identity_from_response(response)?
                }
            };
            self.save_user(identity, String::new(), hash, true).await
        } else {
            let identity = match identity {
                Some(identity) => identity,
                None => self.verify_user_token(token).await?,
            };
            self.save_user(identity, token.to_owned(), hash, false)
                .await
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
            identity,
            access: token,
            web_session,
            baseline: now(),
            imported_token_hash: Some(imported_token_hash),
            revoked: false,
        };
        self.update_secrets(move |secrets, _| {
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
        self.update_secrets(move |secrets, _| {
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
            .update_secrets(move |secrets, _| {
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
        let config = self.imported_token.as_ref().ok_or(Error::Configuration)?;
        let (team, _, _, _) = self.project.as_ref().ok_or(Error::Configuration)?;
        config
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

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
