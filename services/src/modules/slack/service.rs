use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, Notify, watch};
use url::Url;
use uuid::Uuid;

use crate::modules::{
    config::service::ConfigService, notification::NotificationService,
    password::service::PasswordService,
};

use super::{
    client::{SlackClient, WebSession},
    mailbox::UserDirectory,
    projects::{Backend, SlackTeam},
    protocol::{Error, Identity, valid_id},
    status::ProjectsStatus,
    store::{Account, Cache, Keychain, Secrets, SendState, random},
};

pub(super) struct Authorization {
    pub state: &'static str,
    pub expires: u64,
    pub failure: Option<Error>,
    verifier: String,
    csrf: Option<String>,
    client_id: String,
    redirect: String,
}

impl Authorization {
    fn json(&self, id: Uuid) -> Value {
        let status = if self.state == "pending" && self.expires <= now() {
            "expired"
        } else {
            self.state
        };
        let expires = OffsetDateTime::from_unix_timestamp(self.expires as i64)
            .ok()
            .and_then(|time| time.format(&Rfc3339).ok());
        json!({"api_version":"1","id":id,"state":status,"expires_at":expires,"error":self.failure.map(Error::object)})
    }
}

#[derive(Default)]
pub(super) struct State {
    pub initialized: bool,
    pub credentials_locked: bool,
    pub storage_unavailable: bool,
    pub secrets: Secrets,
    pub app_token: Option<String>,
    pub cache: Cache,
    pub socket_state: &'static str,
    pub issue: Option<Error>,
    retired: bool,
    authorizations: BTreeMap<Uuid, Authorization>,
    active_authorization: Option<Uuid>,
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
    pub(super) refresh_lock: Mutex<()>,
    pub(super) user_directory: Mutex<Option<UserDirectory>>,
    client_id: Option<&'static str>,
    keychain: Arc<dyn Keychain>,
    cache_path: Option<PathBuf>,
    port: std::sync::OnceLock<u16>,
    project: Option<(String, SlackTeam, Uuid, Arc<ProjectsStatus>)>,
    password: Option<(Arc<PasswordService>, String)>,
    password_operation: Mutex<()>,
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
        let namespace = format!(
            "{id}-{}-{}",
            backend.client_id.unwrap_or_default(),
            settings.team_id.as_deref().unwrap_or_default()
        );
        let mut service = Self::with_dependencies(
            backend.client_id,
            notification,
            (backend.keychain)(namespace.clone()),
            &backend.api,
            backend.path.clone(),
        );
        service.cache_path = service
            .cache_path
            .map(|path| path.with_file_name(format!("slack-cache-{namespace}.json")));
        service.password = backend
            .passwords
            .as_ref()
            .zip(settings.password_id.as_ref())
            .map(|(passwords, id)| (Arc::clone(passwords), id.clone()));
        if settings.token.is_some() {
            service.imported_token = Some(config);
        }
        service.project = Some((id, settings, generation, reporter));
        service
    }

    pub(super) fn with_dependencies(
        client_id: Option<&'static str>,
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
            refresh_lock: Mutex::new(()),
            user_directory: Mutex::default(),
            client_id,
            keychain,
            cache_path: service_path
                .as_ref()
                .map(|path| path.with_file_name("slack-cache.json")),
            port: std::sync::OnceLock::new(),
            project: None,
            password: None,
            password_operation: Mutex::new(()),
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
                        && secrets
                            .account
                            .as_ref()
                            .is_some_and(|account| account.imported_token_hash.is_some())
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
                    if state.app_token.is_none() {
                        state.app_token.clone_from(&secrets.app_token);
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

    pub(super) fn set_port(&self, port: u16) {
        let _ = self.port.set(port);
    }

    pub(super) fn signal_change(&self) {
        self.change.send_modify(|generation| *generation += 1);
        self.alerts.notify_one();
    }

    pub(super) async fn retire(&self) {
        let mut state = self.state.lock().await;
        state.retired = true;
        state.active_authorization = None;
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
            _ if state.app_token.is_none() => "unconfigured",
            _ => "authorization_required",
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
               "password_id":settings.password_id,
               "app_token_set":state.app_token.is_some(),
               "authorization_state":Self::authorization_state(&state),
               "socket_state":state.socket_state,
               "issue_code":issue.map(|error| error.object()["code"].clone())})
    }

    pub(super) async fn publish_status(&self) {
        if let Some((id, _, generation, reporter)) = &self.project {
            reporter.update(id, *generation, self.public_status().await);
        }
    }

    pub(super) fn valid_app_token(token: &str) -> bool {
        token.len() <= 512
            && token.len() > 5
            && token.starts_with("xapp-")
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }

    pub(super) async fn restore_app_token(&self) {
        let Some((passwords, id)) = &self.password else {
            return;
        };
        let _operation = self.password_operation.lock().await;
        let result = passwords.fetch(id).await;
        let mut state = self.state.lock().await;
        if state.retired {
            return;
        }
        match result {
            Ok(secret) if Self::valid_app_token(secret.trim()) => {
                state.app_token = Some(secret.trim().to_owned());
                state.issue = None;
                drop(state);
                self.signal_change();
            }
            Err(crate::modules::password::service::PasswordError::EntryNotFound) => {}
            _ if state.app_token.is_none() => {
                state.issue = Some(Error::PasswordRead);
            }
            _ => {}
        }
    }

    pub(super) async fn begin_authorization(&self) -> Result<(Uuid, Value), Error> {
        let client_id = self.client_id.ok_or(Error::AppClientId)?;
        let port = self.port.get().ok_or(Error::Configuration)?;
        let redirect = format!("http://localhost:{port}/slack/oauth/callback");
        let mut state = self.state.lock().await;
        if state.retired {
            return Err(Error::Project);
        }
        if state.credentials_locked {
            return Err(Error::Credentials);
        }
        let csrf = if let Some((project, _, _, _)) = &self.project {
            format!("{project}.{}", random()?)
        } else {
            random()?
        };
        let verifier = random()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = Url::parse("https://slack.com/oauth/v2/authorize").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("client_id", client_id),
            ("scope", ""),
            (
                "user_scope",
                "im:read,im:history,mpim:read,mpim:history,channels:read,channels:history,groups:read,groups:history,chat:write,users:read",
            ),
            ("redirect_uri", &redirect),
            ("state", &csrf),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ]);
        if let Some(team) = self
            .project
            .as_ref()
            .and_then(|(_, settings, _, _)| settings.team_id.as_deref())
        {
            url.query_pairs_mut().append_pair("team", team);
        }
        if let Some(active) = state.active_authorization
            && let Some(record) = state.authorizations.get_mut(&active)
            && record.state == "pending"
        {
            record.state = "superseded";
            record.csrf = None;
        }
        let now = now();
        state
            .authorizations
            .retain(|_, record| record.expires.saturating_add(600) > now);
        if state.authorizations.len() >= 64
            && let Some(oldest) = state
                .authorizations
                .iter()
                .min_by_key(|(_, record)| record.expires)
                .map(|(id, _)| *id)
        {
            state.authorizations.remove(&oldest);
        }
        let id = Uuid::new_v4();
        let record = Authorization {
            state: "pending",
            expires: now + 600,
            failure: None,
            verifier,
            csrf: Some(csrf),
            client_id: client_id.to_owned(),
            redirect,
        };
        let mut result = record.json(id);
        result["authorization_url"] = json!(url.as_str());
        state.authorizations.insert(id, record);
        state.active_authorization = Some(id);
        Ok((id, result))
    }

    pub(super) async fn authorization(&self, id: Uuid) -> Result<Value, Error> {
        let state = self.state.lock().await;
        let record = state
            .authorizations
            .get(&id)
            .filter(|record| record.expires.saturating_add(600) > now())
            .ok_or(Error::AuthorizationRecord)?;
        Ok(record.json(id))
    }

    pub(super) async fn callback(
        &self,
        host: &str,
        csrf: &str,
        code: Option<String>,
        denied: bool,
    ) -> Result<(), Error> {
        let port = self.port.get().ok_or(Error::OAuthState)?;
        let expected_length = self
            .project
            .as_ref()
            .map_or(43, |(project, _, _, _)| project.len() + 44);
        if host != format!("localhost:{port}") || csrf.len() != expected_length {
            return Err(Error::OAuthState);
        }
        let (id, client_id, redirect, verifier) = {
            let mut state = self.state.lock().await;
            let id = state.active_authorization.ok_or(Error::OAuthState)?;
            let record = state.authorizations.get_mut(&id).ok_or(Error::OAuthState)?;
            if record.state != "pending"
                || record.expires <= now()
                || !record
                    .csrf
                    .as_ref()
                    .is_some_and(|value| bool::from(value.as_bytes().ct_eq(csrf.as_bytes())))
            {
                return Err(Error::OAuthState);
            }
            record.csrf = None;
            (
                id,
                record.client_id.clone(),
                record.redirect.clone(),
                std::mem::take(&mut record.verifier),
            )
        };
        let result = match code.filter(|code| !denied && !code.is_empty()) {
            Some(code) => {
                self.exchange(id, &client_id, &redirect, &verifier, &code)
                    .await
            }
            None => Err(Error::Permission),
        };
        let mut state = self.state.lock().await;
        if let Some(record) = state.authorizations.get_mut(&id)
            && record.state == "pending"
        {
            record.state = if result.is_ok() {
                "succeeded"
            } else {
                "failed"
            };
            record.failure = result.err();
        }
        Ok(())
    }

    async fn exchange(
        &self,
        id: Uuid,
        client_id: &str,
        redirect: &str,
        verifier: &str,
        code: &str,
    ) -> Result<(), Error> {
        let value = self
            .client
            .oauth(&[
                ("client_id", client_id),
                ("redirect_uri", redirect),
                ("code_verifier", verifier),
                ("code", code),
                ("grant_type", "authorization_code"),
            ])
            .await?;
        let token = value["authed_user"]["access_token"]
            .as_str()
            .ok_or(Error::Upstream)?;
        let grant = &value["authed_user"];
        self.save_user(
            self.verify_identity(token).await?,
            token.to_owned(),
            grant["refresh_token"].as_str().map(str::to_owned),
            grant["expires_in"]
                .as_u64()
                .map(|seconds| now().saturating_add(seconds)),
            Some(id),
            None,
            false,
        )
        .await
    }

    pub(super) async fn verify_user_token(&self, token: &str) -> Result<Identity, Error> {
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

    pub(super) async fn use_imported_token(
        &self,
        token: String,
        identity: Identity,
    ) -> Result<(), Error> {
        let _lock = self.imported_token_lock.lock().await;
        self.initialize().await?;
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        self.save_user(identity, token, None, None, None, Some(hash), false)
            .await
    }

    pub(super) async fn restore_imported_token(&self) -> Result<(), Error> {
        let Some(config) = &self.imported_token else {
            return Ok(());
        };
        let _lock = self.imported_token_lock.lock().await;
        let (id, _, _, _) = self.project.as_ref().ok_or(Error::Configuration)?;
        let token = config
            .read_record_password("slack", "teams", id, "token")
            .await
            .map_err(|_| Error::PasswordRead)?
            .trim_end_matches('\n')
            .to_owned();
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
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
        if token.starts_with('{') {
            let session = WebSession::parse(&token)?;
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
            self.save_user(identity, String::new(), None, None, None, Some(hash), true)
                .await
        } else {
            let identity = match identity {
                Some(identity) => identity,
                None => self.verify_user_token(&token).await?,
            };
            self.save_user(identity, token, None, None, None, Some(hash), false)
                .await
        }
    }

    async fn save_user(
        &self,
        identity: Identity,
        token: String,
        refresh: Option<String>,
        expires_at: Option<u64>,
        authorization: Option<Uuid>,
        imported_token_hash: Option<String>,
        web_session: bool,
    ) -> Result<(), Error> {
        let mut account = Account {
            identity,
            access: token,
            web_session,
            refresh,
            expires_at,
            baseline: now(),
            imported_token_hash,
            revoked: false,
        };
        self.update_secrets(move |secrets, state| {
            if authorization.is_some() && state.active_authorization != authorization {
                return Err(Error::OAuthState);
            }
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

    pub(super) async fn disconnect(&self, session: Uuid) -> Result<(), Error> {
        self.update_secrets(move |secrets, _| {
            if secrets
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

    pub(super) async fn access(&self, session: Uuid) -> Result<String, Error> {
        let _refresh = self.refresh_lock.lock().await;
        let account = self.state.lock().await.account(session)?.clone();
        if account
            .expires_at
            .is_none_or(|expires| expires > now().saturating_add(60))
        {
            return Ok(account.access);
        }
        let Some(refresh) = account.refresh.as_deref() else {
            self.revoke(session, &account.access).await;
            return Err(Error::Authorization);
        };
        let client_id = self.client_id.ok_or(Error::AppClientId)?;
        let response = self
            .client
            .oauth(&[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", refresh),
            ])
            .await;
        let value = match response {
            Ok(value) => value,
            Err(error @ Error::Authorization) => {
                self.revoke(session, &account.access).await;
                return Err(error);
            }
            Err(_) if account.expires_at.is_some_and(|expires| expires > now()) => {
                return Ok(account.access);
            }
            Err(error) => return Err(error),
        };
        let fields = value.get("authed_user").unwrap_or(&value);
        let token = fields["access_token"]
            .as_str()
            .ok_or(Error::Upstream)?
            .to_owned();
        let refresh = fields["refresh_token"]
            .as_str()
            .ok_or(Error::Upstream)?
            .to_owned();
        let expires = fields["expires_in"].as_u64().ok_or(Error::Upstream)?;
        self.update_secrets(move |secrets, _| {
            let current = secrets
                .account
                .as_mut()
                .filter(|current| {
                    current.identity.session_id == session
                        && current.access == account.access
                        && !current.revoked
                })
                .ok_or(Error::Session)?;
            current.access = token.clone();
            current.refresh = Some(refresh);
            current.expires_at = Some(now().saturating_add(expires));
            Ok(token)
        })
        .await
    }

    async fn web_session(&self, session: Uuid) -> Result<Option<WebSession>, Error> {
        if !self.state.lock().await.account(session)?.web_session {
            return Ok(None);
        }
        let config = self.imported_token.as_ref().ok_or(Error::Configuration)?;
        let (team, _, _, _) = self.project.as_ref().ok_or(Error::Configuration)?;
        let secret = config
            .read_record_password("slack", "teams", team, "token")
            .await
            .map_err(|_| Error::PasswordRead)?;
        Ok(Some(WebSession::parse(&secret)?))
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
        let token = self.access(session).await?;
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
        let token = self.access(session).await?;
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
