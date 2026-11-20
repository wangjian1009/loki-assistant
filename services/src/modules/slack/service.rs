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

use crate::modules::notification::NotificationService;

use super::{
    client::SlackClient,
    password::SlackPasswords,
    projects::{Backend, SlackTeam},
    protocol::{Error, Identity, valid_id},
    status::ProjectsStatus,
    store::{Account, Cache, Keychain, Secrets, SendState, local_token, random},
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
    pub local_token: Option<String>,
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
        if account.revoked {
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
    client_id: Option<&'static str>,
    keychain: Arc<dyn Keychain>,
    cache_path: Option<PathBuf>,
    token_path: Option<PathBuf>,
    port: std::sync::OnceLock<u16>,
    project: Option<(String, SlackTeam, Uuid, Arc<ProjectsStatus>)>,
    password: Option<(Arc<SlackPasswords>, String)>,
    password_operation: Mutex<()>,
    authorization_changed: Notify,
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
            client_id,
            keychain,
            cache_path: service_path
                .as_ref()
                .map(|path| path.with_file_name("slack-cache.json")),
            token_path: service_path.map(|path| path.with_file_name("slack-api-token")),
            port: std::sync::OnceLock::new(),
            project: None,
            password: None,
            password_operation: Mutex::new(()),
            authorization_changed: Notify::new(),
        }
    }

    pub(super) async fn initialize(&self) -> Result<(), Error> {
        let keychain = Arc::clone(&self.keychain);
        let cache_path = self.cache_path.clone();
        let token_path = self.token_path.clone();
        let mut state = Arc::clone(&self.state).lock_owned().await;
        tokio::task::spawn_blocking(move || {
            if !state.initialized {
                let token = token_path
                    .as_deref()
                    .ok_or(Error::Storage)
                    .and_then(local_token);
                let cache = cache_path
                    .as_deref()
                    .ok_or(Error::Storage)
                    .and_then(Cache::load);
                state.local_token = token.ok();
                match cache {
                    Ok(cache) => state.cache = cache,
                    Err(_) => state.storage_unavailable = true,
                }
                state.initialized = true;
            }
            match keychain.load() {
                Ok(secrets) => {
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

    pub(super) fn notification_path(&self) -> Option<String> {
        self.project
            .as_ref()
            .map(|(project, _, _, _)| format!("/slack/teams/{project}/open"))
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
            keychain.save(&next)?;
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

    pub(super) async fn verify_local(&self, token: Option<&str>) -> Result<(), Error> {
        let state = self.state.lock().await;
        if state.retired {
            return Err(Error::Project);
        }
        let expected = state.local_token.as_deref().ok_or(Error::Storage)?;
        if token.is_some_and(|token| bool::from(token.as_bytes().ct_eq(expected.as_bytes()))) {
            Ok(())
        } else {
            Err(Error::LocalAuth)
        }
    }

    fn authorization_state(state: &State, configured: bool) -> &'static str {
        let account = state.secrets.account.as_ref();
        match account {
            _ if state.credentials_locked => "credentials_unavailable",
            Some(account) if account.revoked => "reauthorization_required",
            Some(_) => "authorized",
            None if !configured || state.app_token.is_none() => "unconfigured",
            None => "authorization_required",
        }
    }

    pub(super) async fn public_status(&self) -> Value {
        let configured = self.client_id.is_some();
        let state = self.state.lock().await;
        let account = state
            .secrets
            .account
            .as_ref()
            .filter(|account| !state.credentials_locked && !account.revoked);
        let (id, name, team_id) = self
            .project
            .as_ref()
            .map(|(id, settings, _, _)| {
                (
                    id.as_str(),
                    settings.name.as_str(),
                    settings.team_id.as_deref(),
                )
            })
            .unwrap_or(("default", "Slack", None));
        let issue = if state.storage_unavailable {
            Some(Error::Storage)
        } else {
            state.issue
        };
        json!({"id":id,"name":name,"team_id":team_id.or_else(|| account.map(|account| account.identity.team_id.as_str())),
               "team_name":account.map(|account| &account.identity.team_name),
               "password_id":self.project.as_ref().and_then(|(_, settings, _, _)| settings.password_id.as_ref()),
               "app_token_set":state.app_token.is_some(),
               "authorization_state":Self::authorization_state(&state, configured),
               "socket_state":state.socket_state,
               "issue_code":issue.map(|error| error.object()["code"].clone())})
    }

    pub(super) async fn publish_status(&self) {
        if let Some((id, _, generation, reporter)) = &self.project {
            reporter.update(id, *generation, self.public_status().await);
        }
    }

    pub(super) async fn status(&self) -> Value {
        let configured = self.client_id.is_some();
        let state = self.state.lock().await;
        let account = state.secrets.account.as_ref();
        let issue = if state.storage_unavailable {
            Some(Error::Storage)
        } else {
            state.issue
        };
        json!({"api_version":"1", "authorization_state":Self::authorization_state(&state, configured),"socket_state":state.socket_state,
               "session":account.filter(|account| !account.revoked && !state.credentials_locked).map(|account| &account.identity),
               "issue":issue.map(Error::object)})
    }

    pub(super) fn valid_app_token(token: &str) -> bool {
        token.len() <= 512
            && token.len() > 5
            && token.starts_with("xapp-")
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }

    pub(super) async fn app_token(&self, token: String) -> Result<(), Error> {
        if !Self::valid_app_token(&token) {
            return Err(Error::Invalid);
        }
        let _operation = self.password_operation.lock().await;
        if self.password.is_none() && self.state.lock().await.app_token.as_ref() == Some(&token) {
            return Ok(());
        }
        self.client.verify_app_token(&token).await?;
        if let Some((passwords, id)) = &self.password {
            passwords
                .save(id, &token)
                .await
                .map_err(|_| Error::PasswordWrite)?;
            let mut state = self.state.lock().await;
            if state.retired {
                return Err(Error::Project);
            }
            state.app_token = Some(token);
            state.issue = None;
            drop(state);
            self.signal_change();
            self.publish_status().await;
            return Ok(());
        }
        let token = self
            .update_secrets(move |secrets, _| {
                secrets.app_token = Some(token.clone());
                Ok(token)
            })
            .await?;
        self.state.lock().await.app_token = Some(token);
        self.signal_change();
        self.publish_status().await;
        Ok(())
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

    pub(super) async fn check_app_token(&self) -> Result<(), Error> {
        let (passwords, id) = self.password.as_ref().ok_or(Error::PasswordRead)?;
        let _operation = self.password_operation.lock().await;
        if self.state.lock().await.retired {
            return Err(Error::Project);
        }
        let secret = passwords.fetch(id).await.map_err(|_| Error::PasswordRead)?;
        let token = secret.trim();
        if !Self::valid_app_token(token) {
            return Err(Error::AppToken);
        }
        self.client.verify_app_token(token).await?;
        let mut state = self.state.lock().await;
        if state.retired {
            return Err(Error::Project);
        }
        let changed = state.app_token.as_deref() != Some(token);
        state.app_token = Some(token.to_owned());
        state.issue = None;
        drop(state);
        if changed {
            self.signal_change();
        }
        self.publish_status().await;
        Ok(())
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
                "im:read,im:history,mpim:read,mpim:history,chat:write,users:read",
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
        self.authorization_changed.notify_waiters();
        Ok((id, result))
    }

    pub(super) async fn credential_id(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .secrets
            .account
            .as_ref()
            .filter(|account| !account.revoked)
            .map(|account| account.identity.team_id.clone())
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

    pub(super) async fn authorization_result(&self, id: Uuid) -> Option<Result<(), Error>> {
        let state = self.state.lock().await;
        let Some(record) = state.authorizations.get(&id) else {
            return Some(Err(Error::OAuthState));
        };
        match record.state {
            "succeeded" => Some(Ok(())),
            "pending" if record.expires > now() => None,
            "pending" => Some(Err(Error::AuthorizationTimeout)),
            _ => Some(Err(record.failure.unwrap_or(Error::OAuthState))),
        }
    }

    pub(super) async fn wait_authorization(&self, id: Uuid) -> Result<(), Error> {
        tokio::time::timeout(std::time::Duration::from_secs(600), async {
            loop {
                let changed = self.authorization_changed.notified();
                if let Some(result) = self.authorization_result(id).await {
                    return result;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or(Err(Error::AuthorizationTimeout))
    }

    pub(super) async fn cancel_authorization(&self, id: Uuid) {
        let mut state = self.state.lock().await;
        if state.active_authorization == Some(id)
            && state
                .authorizations
                .get(&id)
                .is_some_and(|record| record.state == "pending")
        {
            state.active_authorization = None;
            state.authorizations.remove(&id);
        }
        self.authorization_changed.notify_waiters();
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
        self.authorization_changed.notify_waiters();
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
        let identity = self.client.test_identity(token).await?;
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
        let grant = &value["authed_user"];
        let mut account = Account {
            identity: Identity {
                session_id: Uuid::new_v4(),
                team_id: team,
                user_id: user,
                team_name: name,
            },
            access: token.to_owned(),
            refresh: grant["refresh_token"].as_str().map(str::to_owned),
            expires_at: grant["expires_in"]
                .as_u64()
                .map(|seconds| now().saturating_add(seconds)),
            baseline: now(),
            revoked: false,
        };
        self.update_secrets(move |secrets, state| {
            if state.active_authorization != Some(id) {
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
            Ok(())
        })
        .await?;
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

    pub(super) async fn slack_get(
        &self,
        session: Uuid,
        method: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, Error> {
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
