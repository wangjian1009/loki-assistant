use crate::process::Process;

use std::{
    fmt,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, Semaphore, watch};
use uuid::Uuid;

use crate::modules::{
    alert::{AlertInput, AlertLevel, AlertService},
    config::service::{ConfigService, ConfigServiceError},
    notification::NotificationService,
};

use super::{
    client::{SlackClient, WebSession},
    disk::DiskCache,
    downloads::Downloads,
    mailbox::UserDirectory,
    picker::PickerItem,
    projects::{Backend, SlackTeam, SlackWatch, read_watches},
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
    nicknames: Nicknames,
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
    pub(super) state: Process<State>,
    pub(super) cache_root: Option<PathBuf>,
    pub(super) downloads: Arc<Downloads>,
    pub(super) clean_cache: Notify,
    pub(super) client: SlackClient,
    alert: Arc<AlertService>,
    pub(super) notification: Arc<NotificationService>,
    pub(super) change: watch::Sender<u64>,
    pub(super) outgoing: Notify,
    pub(super) sync: Notify,
    pub(super) alerts: Notify,
    pub(super) user_fetch: Semaphore,
    pub(super) directory_fetch: Semaphore,
    pub(super) picker_changed: Notify,
    pub(super) avatars_changed: Notify,
    pub(super) preview_downloads: Semaphore,
    pub(super) subscriptions_changed: Notify,
    pub(super) watch_changes: Semaphore,
    pub(super) project: (String, SlackTeam, Uuid, Arc<ProjectsStatus>),
    pub(super) config: Arc<ConfigService>,
    credential_import: Semaphore,
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
        alert: Arc<AlertService>,
        notification: Arc<NotificationService>,
        id: String,
        settings: SlackTeam,
        generation: Uuid,
        reporter: Arc<ProjectsStatus>,
        backend: &Backend,
        config: Arc<ConfigService>,
        downloads: Arc<Downloads>,
        nicknames: Nicknames,
    ) -> Self {
        let state = Process::start(State {
            socket_state: "stopped",
            nicknames,
            ..State::default()
        });
        Self {
            state,
            cache_root: backend.cache_root.clone(),
            client: SlackClient::new(&backend.api),
            alert,
            notification,
            change: watch::channel(0).0,
            outgoing: Notify::new(),
            sync: Notify::new(),
            alerts: Notify::new(),
            user_fetch: Semaphore::new(1),
            directory_fetch: Semaphore::new(1),
            picker_changed: Notify::new(),
            avatars_changed: Notify::new(),
            preview_downloads: Semaphore::new(4),
            subscriptions_changed: Notify::new(),
            watch_changes: Semaphore::new(1),
            downloads,
            clean_cache: Notify::new(),
            project: (id, settings, generation, reporter),
            config,
            credential_import: Semaphore::new(1),
        }
    }

    pub(super) async fn team_nicknames(
        &self,
        team: &str,
    ) -> std::collections::BTreeMap<String, String> {
        super::projects::read_team_nicknames(&self.config, team)
            .await
            .unwrap_or_default()
    }

    pub(super) async fn watching(&self, id: &str) -> bool {
        read_watches(&self.config)
            .await
            .is_ok_and(|watches| watches.iter().any(|watch| watch.id == id))
    }

    pub(super) async fn set_watch(
        &self,
        id: String,
        conversation: &str,
        watching: bool,
    ) -> Result<(), Error> {
        let display_name = if watching {
            if read_watches(&self.config)
                .await?
                .iter()
                .any(|watch| watch.id == id)
            {
                return Ok(());
            }
            Some(self.watch_conversation_name(conversation).await?)
        } else {
            None
        };
        let _change = self
            .watch_changes
            .acquire()
            .await
            .expect("Slack 关注许可不能关闭");
        let key = super::WATCHES.to_owned();
        if let Some(display_name) = display_name {
            let value = json!(SlackWatch {
                id: id.clone(),
                display_name
            });
            return match self
                .config
                .replace_record("slack".into(), key, id, value, true, false)
                .await
            {
                Ok(_) | Err(ConfigServiceError::RecordExists) => Ok(()),
                Err(_) => Err(Error::WatchStorage),
            };
        }
        self.config
            .remove_record("slack".into(), key, id.clone())
            .await
            .map_err(|_| Error::WatchStorage)?;
        self.forget_watch(&id).await;
        Ok(())
    }

    pub(super) async fn watch_conversation_name(&self, id: &str) -> Result<String, Error> {
        let session = self.session_id().await.ok_or(Error::Session)?;
        if !self.has_conversation(id).await {
            self.refresh_directory(session).await?;
        }
        let id = id.to_owned();
        self.state
            .call(move |state| {
                state.account(session)?;
                let room = state
                    .cache
                    .conversations
                    .get(&id)
                    .ok_or(Error::Conversation)?;
                if !matches!(room.kind.as_str(), "public_channel" | "private_channel") {
                    return Err(Error::WatchUnsupported);
                }
                Ok(format!("#{}", room.title))
            })
            .await
    }

    pub(super) fn signal_change(&self) {
        self.change.send_modify(|generation| *generation += 1);
        self.alerts.notify_one();
    }

    pub(super) async fn retire(&self) {
        self.state
            .call(|state| {
                state.retired = true;
                state.cache.clear_subscriptions();
            })
            .await;
        self.signal_change();
    }

    pub(super) async fn nicknames_changed(&self, nicknames: Nicknames) {
        let (id, _, generation, reporter) = self.project.clone();
        self.state
            .call(move |state| {
                state.nicknames = nicknames.clone();
                if let Some(account) = &mut state.secrets.account {
                    account.nicknames = nicknames;
                }

                if let Some(account) = state
                    .cache
                    .session_id
                    .and_then(|session| state.account(session).ok())
                {
                    reporter.update_new_conversations(
                        &id,
                        generation,
                        Some((&state.cache, account, state.user_directory.as_ref())),
                    );
                    for room in state.cache.conversations.values_mut() {
                        if let Some(journal) = &mut room.journal {
                            journal.nicknames_changed();
                        }
                    }
                }
            })
            .await;
        self.picker_changed.notify_one();
    }

    pub(super) async fn forget_watch(&self, watch: &str) {
        let watch = watch.to_owned();
        self.state
            .call(move |state| {
                state
                    .cache
                    .notifications
                    .retain(|_, alert| alert.watch.as_deref() != Some(watch.as_str()));
            })
            .await;
    }

    pub(super) async fn update_cache<T: Send + 'static>(
        &self,
        session: Uuid,
        operation: impl FnOnce(&mut Cache, &Account) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let (_, _, generation, reporter) = &self.project;
        let (generation, reporter) = (*generation, Arc::clone(reporter));
        let report = reporter.clone();
        let result = self
            .state
            .blocking(move |state| {
                state.account(session)?;
                let State {
                    cache,
                    secrets,
                    user_directory,
                    ..
                } = state;
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
            .await;
        if result.is_ok() {
            report.flush().await;
            self.picker_changed.notify_one();
        }
        result
    }

    async fn update_secrets<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Secrets, &Nicknames) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let root = self.cache_root.clone();
        let (id, _, generation, reporter) = self.project.clone();
        let result = self
            .state
            .blocking(move |state| {
                if state.retired {
                    return Err(Error::Project);
                }
                let result = operation(&mut state.secrets, &state.nicknames)?;
                let identity = state
                    .secrets
                    .account
                    .as_ref()
                    .map(|account| account.identity.clone());
                let session = identity.as_ref().map(|identity| identity.session_id);
                let authorization_lost =
                    session.is_some_and(|session| state.account(session).is_err());
                if state.cache.session_id != session || authorization_lost {
                    reporter.update_new_conversations(&id, generation, None);
                }
                if state.cache.session_id != session {
                    state.user_directory = None;
                    state.picker_items = Arc::default();
                    state.picker_failed = false;
                    state.cache.reset(session);
                    if let (Some(root), Some(identity)) = (root, identity)
                        && let Ok(disk) =
                            DiskCache::open(&root, &identity.team_id, &identity.user_id)
                    {
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
                                &id,
                                generation,
                                Some((&state.cache, account, state.user_directory.as_ref())),
                            );
                        }
                    }
                } else if authorization_lost {
                    state.cache.clear_subscriptions();
                }
                Ok(result)
            })
            .await;
        self.picker_changed.notify_one();
        result
    }

    pub(super) async fn has_conversation(&self, id: &str) -> bool {
        let id = id.to_owned();
        self.state
            .call(move |state| state.cache.conversations.contains_key(&id))
            .await
    }

    pub(super) async fn session_token(&self, session: Uuid) -> Result<String, Error> {
        self.state
            .call(move |state| Ok(state.account(session)?.access.clone()))
            .await
    }

    pub(super) async fn check_session(&self, session: Uuid) -> Result<(), Error> {
        self.state
            .call(move |state| state.account(session).map(|_| ()))
            .await
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
        self.state
            .call(move |state| {
                let session = state.secrets.account.as_ref()?.identity.session_id;
                state.account(session).ok().map(|_| session)
            })
            .await
    }

    pub(super) async fn needs_reauthorization(&self) -> bool {
        self.state
            .call(|state| Self::authorization_state(state) == "reauthorization_required")
            .await
    }

    pub(super) async fn public_status(&self) -> Value {
        let (team_id, settings, _, _) = self.project.clone();
        self.state
            .call(move |state| {
                let account = state.secrets.account.as_ref().filter(|account| {
                    !account.revoked && (!account.access.is_empty() || account.web_session)
                });
                json!({"team_id":team_id,"name":settings.name,
               "team_name":account.map(|account| &account.identity.team_name),
               "session_id":account.map(|account| account.identity.session_id),
               "user_id":account.map(|account| format!("{team_id}.{}", account.identity.user_id)),
               "authorization_state":Self::authorization_state(state),
               "socket_state":state.socket_state,
               "issue_code":state.issue.map(|error| error.object()["code"].clone())})
            })
            .await
    }

    pub(super) async fn publish_status(&self) {
        let (id, _, generation, reporter) = &self.project;
        reporter
            .update(id, *generation, self.public_status().await)
            .await;
    }

    fn reauthorization_alert_id(&self) -> String {
        format!("slack.{}.reauthorization", self.project.0)
    }

    pub(super) async fn sync_reauthorization_alert(&self) {
        let (id, settings, _, _) = self.project.clone();
        let alert = self.alert.clone();
        let alert_id = self.reauthorization_alert_id();
        self.state
            .with(move |state| {
                Box::pin(async move {
                    if state.retired {
                        return;
                    }
                    match Self::authorization_state(state) {
                        "reauthorization_required" => {
                            let result = alert
                                .publish(
                                    &alert_id,
                                    AlertInput {
                                        title: "Slack 需要重新授权".into(),
                                        body: Some(format!(
                                            "点击为「{}」重新填写 Slack 凭据。",
                                            settings.name
                                        )),
                                        app: Some("slack".into()),
                                        callback_url: Some(super::routes::credential_reset_path(
                                            &id,
                                        )),
                                        level: AlertLevel::Warning,
                                    },
                                )
                                .await;
                            if let Err(error) = result {
                                eprintln!("无法发布 Slack 工作区 {id} 的重新授权告警：{error}");
                            }
                        }
                        "authorized" => {
                            if let Err(error) = alert.clear(&alert_id).await {
                                eprintln!("无法清理 Slack 工作区 {id} 的重新授权告警：{error}");
                            }
                        }
                        _ => {}
                    }
                })
            })
            .await;
    }

    pub(super) async fn clear_reauthorization_alert(&self) {
        if let Err(error) = self.alert.clear(&self.reauthorization_alert_id()).await {
            eprintln!(
                "无法清理 Slack 工作区 {} 的重新授权告警：{error}",
                self.project.0
            );
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
        let _lock = self
            .credential_import
            .acquire()
            .await
            .expect("Slack 操作许可不能关闭");
        let passwords = self.read_passwords().await?;
        let token = imported_token(&passwords)?;
        let hash = imported_token_hash(token, &passwords);
        let result = self.restore_credentials(token, &passwords, &hash).await;
        self.state
            .call(move |state| {
                state.rejected_credentials = (result == Err(Error::Authorization)).then_some(hash);
            })
            .await;
        result
    }

    async fn restore_credentials(
        &self,
        token: &str,
        passwords: &[Option<String>; 2],
        hash: &str,
    ) -> Result<(), Error> {
        let imported_hash = hash.to_owned();
        let decision = self
            .state
            .call(move |state| {
                if state.secrets.disabled_token.as_deref() == Some(&imported_hash) {
                    return Ok(None);
                }
                let previous = state.secrets.account.as_ref().filter(|account| {
                    account.imported_token_hash.as_deref() == Some(&imported_hash)
                });
                if previous.is_some_and(|account| account.revoked) {
                    return Err(Error::Authorization);
                }
                if previous.is_some_and(|account| !account.access.is_empty() || account.web_session)
                {
                    return Ok(None);
                }
                Ok(Some(previous.map(|account| account.identity.clone())))
            })
            .await?;
        let Some(identity) = decision else {
            return Ok(());
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

        self.state
            .call(move |state| {
                let resettable = match imported_token(&passwords) {
                    Ok(token) => {
                        let hash = imported_token_hash(token, &passwords);
                        state.rejected_credentials.as_deref() == Some(&hash)
                            || state.secrets.account.as_ref().is_some_and(|account| {
                                account.revoked
                                    && account.imported_token_hash.as_deref() == Some(&hash)
                            })
                    }
                    Err(_) => state.issue == Some(Error::WebSessionIncomplete),
                };
                if resettable {
                    Ok(passwords)
                } else {
                    Err(Error::CredentialsCurrent)
                }
            })
            .await
    }

    async fn save_user(
        &self,
        identity: Identity,
        token: String,
        imported_token_hash: String,
        web_session: bool,
    ) -> Result<(), Error> {
        let mut account = Account {
            nicknames: Nicknames::default(),
            identity,
            access: token,
            web_session,
            baseline: now(),
            imported_token_hash: Some(imported_token_hash),
            revoked: false,
        };
        self.update_secrets(move |secrets, nicknames| {
            account.nicknames = nicknames.clone();
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
        self.state.call(|state| state.issue = None).await;
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
            self.sync_reauthorization_alert().await;
            self.publish_status().await;
            self.signal_change();
        }
    }

    pub(super) async fn web_session(&self, session: Uuid) -> Result<Option<WebSession>, Error> {
        if !self
            .state
            .call(move |state| Ok(state.account(session)?.web_session))
            .await?
        {
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
        let token = self.session_token(session).await?;
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
        let token = self.session_token(session).await?;
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
