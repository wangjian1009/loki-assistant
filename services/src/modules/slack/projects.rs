use std::{
    collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration,
};

use axum::Router;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Mutex, Notify, RwLock, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    modules::{
        config::service::{
            ActionFuture, ChangeFuture, ConfigChange, ConfigChangeHandler, ConfigService,
            ConfigServiceError,
        },
        notification::NotificationService,
        password::service::{PasswordService, validate_file},
        status::StatusService,
    },
    runtime::ServicePfile,
};

use super::{
    mailbox::{next_cursor, read_cursor},
    protocol::{Error, Page},
    routes,
    service::SlackService,
    status::ProjectsStatus,
    store::{Keychain, LoginKeychain},
};

pub(super) type BrowserFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub(super) enum RegistrationStatus {
    NeedsCredentials,
    Pending,
    Confirmed,
    Rejected,
}

pub(super) struct Backend {
    pub path: Option<PathBuf>,
    pub api: String,
    pub keychain: Arc<dyn Fn(String) -> Arc<dyn Keychain> + Send + Sync>,
    pub passwords: Option<Arc<PasswordService>>,
    pub open_browser: Arc<dyn Fn(String) -> BrowserFuture + Send + Sync>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(super) struct SlackTeam {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "id")]
    pub legacy_id: Option<String>,
    pub team_id: Option<String>,
    pub password_id: Option<String>,
    pub token: Option<String>,
}

pub(crate) fn valid_project(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(super) fn valid_team_id(id: &str) -> bool {
    (2..=48).contains(&id.len())
        && id.starts_with('T')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn parse_projects(value: &Value) -> Result<BTreeMap<String, SlackTeam>, String> {
    let records: Vec<SlackTeam> = serde_json::from_value(value.clone())
        .map_err(|_| "工作区配置必须包含 name 和 team_id。".to_owned())?;
    let mut projects = BTreeMap::new();
    let mut password_ids = std::collections::BTreeSet::new();
    for mut settings in records {
        let id = settings
            .team_id
            .as_ref()
            .filter(|team| valid_team_id(team))
            .ok_or("Slack Team ID 无效。")?
            .clone();
        if settings.name.trim().is_empty() {
            settings.name.clone_from(&id);
        }
        if settings.name.chars().count() > 80 {
            return Err("请核对工作区显示名称。".into());
        }
        let password = settings.password_id.get_or_insert_with(|| {
            default_password_id(
                settings
                    .legacy_id
                    .as_deref()
                    .filter(|id| valid_project(id))
                    .unwrap_or(&id),
            )
        });
        if validate_file(password).is_err() || !password_ids.insert(password.clone()) {
            return Err("每个工作区需要独立的有效密码条目 ID。".into());
        }
        projects.insert(id, settings);
    }
    Ok(projects)
}

fn default_password_id(project: &str) -> String {
    format!("loki-assistant/slack/{project}/app-token.gpg")
}

struct Project {
    settings: SlackTeam,
    service: Arc<SlackService>,
    worker: JoinHandle<()>,
}

impl Drop for Project {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct TeamDraft {
    generation: Uuid,
    record: Option<Value>,
    processing: bool,
    config: std::sync::Weak<ConfigService>,
    service: Arc<SlackService>,
    outcome: oneshot::Sender<Result<Value, ConfigServiceError>>,
}

pub struct Projects {
    config: Arc<ConfigService>,
    notification: Arc<NotificationService>,
    pub(super) status: Arc<ProjectsStatus>,
    projects: RwLock<BTreeMap<String, Project>>,
    reconcile: Mutex<()>,
    drafts: Mutex<BTreeMap<String, TeamDraft>>,
    port: std::sync::OnceLock<u16>,
    backend: Backend,
    shutdown: Notify,
}

impl std::fmt::Debug for Projects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlackProjects")
            .finish_non_exhaustive()
    }
}

impl Projects {
    pub fn new(
        config: Arc<ConfigService>,
        notification: Arc<NotificationService>,
        status: Arc<StatusService>,
        passwords: Arc<PasswordService>,
    ) -> Self {
        Self::with_backend(
            config,
            notification,
            status,
            Backend {
                path: ServicePfile::path().ok(),
                api: "https://slack.com/api".into(),
                keychain: Arc::new(|account| Arc::new(LoginKeychain(account))),
                passwords: Some(passwords),
                open_browser: Arc::new(|url| {
                    Box::pin(async move {
                        let mut opener = tokio::process::Command::new("/usr/bin/open");
                        opener.arg(url).kill_on_drop(true);
                        let status = tokio::time::timeout(Duration::from_secs(10), opener.output())
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .map(|output| output.status);
                        if status.is_some_and(|status| status.success()) {
                            Ok(())
                        } else {
                            Err("无法在浏览器中打开 Slack 页面。".into())
                        }
                    })
                }),
            },
        )
    }

    pub(super) fn with_backend(
        config: Arc<ConfigService>,
        notification: Arc<NotificationService>,
        status: Arc<StatusService>,
        backend: Backend,
    ) -> Self {
        Self {
            config,
            notification,
            status: Arc::new(ProjectsStatus::new(status)),
            projects: RwLock::default(),
            reconcile: Mutex::default(),
            drafts: Mutex::default(),
            port: std::sync::OnceLock::new(),
            backend,
            shutdown: Notify::new(),
        }
    }

    pub fn router(self: &Arc<Self>) -> Router {
        routes::router(Arc::clone(self))
    }

    pub async fn run(self: Arc<Self>, port: u16) {
        let _ = self.port.set(port);
        let _ = self.sync_projects().await;
        self.shutdown.notified().await;
    }

    pub async fn stop(&self) {
        let _sync = self.reconcile.lock().await;
        let mut projects = self.projects.write().await;
        for (id, project) in projects.iter() {
            project.service.retire().await;
            project.worker.abort();
            self.status.remove(id);
        }
        projects.clear();
        self.shutdown.notify_one();
    }

    pub(super) async fn service(&self, team_id: &str) -> Result<Arc<SlackService>, Error> {
        self.projects
            .read()
            .await
            .get(team_id)
            .map(|project| Arc::clone(&project.service))
            .ok_or(Error::Project)
    }

    async fn active_sessions(
        &self,
        team_id: Option<&str>,
    ) -> Result<(Vec<(String, Arc<SlackService>, Uuid)>, String), Error> {
        let services: Vec<_> = if let Some(team_id) = team_id {
            vec![(team_id.to_owned(), self.service(team_id).await?)]
        } else {
            self.projects
                .read()
                .await
                .iter()
                .map(|(id, project)| (id.clone(), Arc::clone(&project.service)))
                .collect()
        };
        let mut sessions = Vec::new();
        let mut fingerprint = Sha256::new();
        for (id, service) in services {
            if let Some(session) = service.session_id().await {
                fingerprint.update(id.as_bytes());
                fingerprint.update(session.as_bytes());
                sessions.push((id, service, session));
            }
        }
        Ok((sessions, format!("{:x}", fingerprint.finalize())))
    }

    pub(super) async fn users(&self, page: Page) -> Result<Value, Error> {
        let limit = page.limit()?;
        let (sessions, fingerprint) = self.active_sessions(None).await?;
        let target = format!("users:{fingerprint}");
        let anchor = read_cursor(&page, Uuid::nil(), &target)?;
        let anchor = anchor.as_deref().unwrap_or_default();
        let results = futures_util::future::join_all(
            sessions
                .iter()
                .map(|(id, service, session)| async move { (id, service.users(*session).await) }),
        )
        .await;
        let mut directories = Vec::new();
        let mut issues = Vec::new();
        let mut first_error = None;
        for (id, result) in results {
            match result {
                Ok(users) => directories.push(users),
                Err(error) => {
                    first_error.get_or_insert(error);
                    issues.push(json!({"team_id":id,"error":error.object()}));
                }
            }
        }
        if directories.is_empty()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        let mut users: Vec<_> = directories
            .iter()
            .flat_map(|directory| directory.iter())
            .filter(|user| user.id.as_str() > anchor)
            .collect();
        users.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let next_cursor = (users.len() > limit)
            .then(|| next_cursor(Uuid::nil(), target, users[limit - 1].id.clone()));
        users.truncate(limit);
        Ok(json!({"api_version":"1","items":users,"next_cursor":next_cursor,"issues":issues}))
    }

    pub(super) async fn conversations(
        &self,
        team_id: Option<&str>,
        page: Page,
    ) -> Result<Value, Error> {
        let limit = page.limit()?;
        let unread = page.unread.unwrap_or_default();
        let kind = page.kind.as_deref().unwrap_or_default();
        if !matches!(
            kind,
            "" | "im" | "mpim" | "public_channel" | "private_channel"
        ) {
            return Err(Error::Invalid);
        }
        let (sessions, fingerprint) = self.active_sessions(team_id).await?;
        if team_id.is_some() && sessions.is_empty() {
            return Err(Error::Authorization);
        }
        let target = format!(
            "conversations:{}:{kind}:{unread}:{fingerprint}",
            team_id.unwrap_or_default()
        );
        // 排序键同时包含工作区和会话 ID，允许相同会话 ID 出现在不同工作区。
        let anchor = read_cursor(&page, Uuid::nil(), &target)?
            .map(|value| {
                serde_json::from_str::<(Option<(u64, u32)>, String, String)>(&value)
                    .map_err(|_| Error::Cursor)
            })
            .transpose()?;
        let results = futures_util::future::join_all(sessions.iter().map(
            |(id, service, session)| async move {
                (id, service.conversation_rows(*session, unread, kind).await)
            },
        ))
        .await;
        let mut items = Vec::new();
        let mut live = true;
        let mut issues = Vec::new();
        let mut first_error = None;
        for (id, result) in results {
            match result {
                Ok((rows, connected)) => {
                    live &= connected;
                    items.extend(
                        rows.into_iter()
                            .map(|row| ((row.order, id, row.id), row.value)),
                    );
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                    issues.push(json!({"team_id":id,"error":error.object()}));
                    live = false;
                }
            }
        }
        if issues.len() == sessions.len()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        let compare = |left: &(Option<(u64, u32)>, &String, String),
                       right: &(Option<(u64, u32)>, &String, String)| {
            right
                .0
                .cmp(&left.0)
                .then(left.1.cmp(right.1))
                .then(left.2.cmp(&right.2))
        };
        if let Some((order, team, conversation)) = anchor {
            let anchor = (order, &team, conversation);
            items.retain(|(key, _)| compare(key, &anchor).is_gt());
        }
        items.sort_by(|(left, _), (right, _)| compare(left, right));
        let next_cursor = if items.len() > limit {
            let key = serde_json::to_string(&items[limit - 1].0).expect("Slack 游标可序列化");
            Some(next_cursor(Uuid::nil(), target, key))
        } else {
            None
        };
        let items: Vec<_> = items
            .into_iter()
            .take(limit)
            .map(|(_, value)| value)
            .collect();
        Ok(
            json!({"api_version":"1","items":items,"next_cursor":next_cursor,
                  "sync_state":if live { "live" } else { "cached" },"issues":issues}),
        )
    }

    pub(super) async fn accept_token(&self, team_id: &str, token: String) -> Result<(), Error> {
        let (service, config, record) = {
            let mut drafts = self.drafts.lock().await;
            let draft = drafts
                .get_mut(team_id)
                .filter(|draft| !draft.processing)
                .ok_or(Error::ImportExpired)?;
            draft.processing = true;
            (
                Arc::clone(&draft.service),
                draft.config.clone(),
                draft.record.take(),
            )
        };
        let (outcome, failure) = match service.verify_user_token(&token).await {
            Ok(identity) => {
                let saved = async {
                    let config = config.upgrade().ok_or(ConfigServiceError::ActionNotFound)?;
                    config
                        .save_record_password(
                            "slack",
                            "teams",
                            team_id,
                            "token",
                            record,
                            token.clone(),
                        )
                        .await
                }
                .await;
                if saved.is_ok()
                    && let Ok(current) = self.service(team_id).await
                {
                    // 等待启动时的凭据加载，随后安装已校验的令牌。
                    current.initialize().await.ok();
                    let _ = current.use_imported_token(token, identity).await;
                }
                let error = saved.as_ref().err().map(|_| Error::ImportSave);
                (saved, error)
            }
            Err(error) => (Err(Self::slack_rejected("teams", error)), Some(error)),
        };
        if let Some(draft) = self.drafts.lock().await.remove(team_id) {
            let _ = draft.outcome.send(outcome);
        }
        failure.map_or(Ok(()), Err)
    }

    async fn wait_import(
        &self,
        team_id: &str,
        service: Arc<SlackService>,
        config: std::sync::Weak<ConfigService>,
        record: Option<Value>,
    ) -> Result<Value, ConfigServiceError> {
        let port = self
            .port
            .get()
            .copied()
            .ok_or(ConfigServiceError::ActionNotFound)?;
        let (sender, mut receiver) = oneshot::channel();
        let generation = Uuid::new_v4();
        {
            let mut drafts = self.drafts.lock().await;
            if drafts.contains_key(team_id) {
                return Err(Self::rejected("teams", "这个工作区已有待完成的令牌导入。"));
            }
            drafts.insert(
                team_id.into(),
                TeamDraft {
                    generation,
                    record,
                    processing: false,
                    config,
                    service,
                    outcome: sender,
                },
            );
        }
        let url = format!("http://127.0.0.1:{port}/slack/token-import?team={team_id}");
        let failure = match (self.backend.open_browser)(url).await {
            Ok(()) => match tokio::time::timeout(Duration::from_secs(600), &mut receiver).await {
                Ok(result) => return result.map_err(|_| ConfigServiceError::ActionNotFound)?,
                Err(_) => "等待 Slack 令牌导入超时。".into(),
            },
            Err(error) => error,
        };
        let mut drafts = self.drafts.lock().await;
        if drafts
            .get(team_id)
            .is_some_and(|draft| draft.generation == generation && !draft.processing)
        {
            drafts.remove(team_id);
            return Err(Self::rejected("teams", failure));
        }
        drop(drafts);
        receiver
            .await
            .map_err(|_| ConfigServiceError::ActionNotFound)?
    }

    fn rejected(key: &str, message: impl Into<String>) -> ConfigServiceError {
        ConfigServiceError::ChangeRejected {
            module_id: "slack".into(),
            key: key.into(),
            message: message.into(),
        }
    }

    fn slack_rejected(key: &str, error: Error) -> ConfigServiceError {
        Self::rejected(
            key,
            error.object()["message"]
                .as_str()
                .unwrap_or("Slack 操作失败。"),
        )
    }

    pub(super) async fn open_team_setup(&self, team: &str) -> Result<(), String> {
        let port = self.port.get().ok_or("Slack 模块尚未启动。")?;
        (self.backend.open_browser)(format!("http://127.0.0.1:{port}/slack/team-setup/{team}"))
            .await
    }

    pub(super) async fn registration_status(&self, id: Uuid, team: &str) -> RegistrationStatus {
        if self
            .config
            .notification_password_already_confirmed(id, "slack", "teams", team, "token")
            .await
        {
            return RegistrationStatus::Confirmed;
        }
        let secret = match self
            .config
            .notification_password(id, "slack", "teams", team, "token")
            .await
        {
            Ok(secret) => secret,
            Err(ConfigServiceError::ActionNotFound) => return RegistrationStatus::NeedsCredentials,
            Err(_) => return RegistrationStatus::Rejected,
        };
        let Ok(session) = super::client::WebSession::parse(&secret) else {
            return RegistrationStatus::Rejected;
        };
        match super::client::SlackClient::new(&self.backend.api)
            .web_call("auth.test", &session, &[], None)
            .await
        {
            Ok(identity)
                if identity["team_id"] == team
                    && identity["user_id"].as_str().is_some()
                    && identity["bot_id"].as_str().is_none() =>
            {
                RegistrationStatus::Confirmed
            }
            Err(Error::Unavailable | Error::RateLimited(_)) => RegistrationStatus::Pending,
            _ => RegistrationStatus::Rejected,
        }
    }

    pub(super) async fn sync_projects(&self) -> Result<(), String> {
        let _sync = self.reconcile.lock().await;
        if self.port.get().is_none() {
            return Ok(());
        }
        let configured = self.config.value("slack", "teams").await.ok();
        let settings = configured
            .as_ref()
            .map(|value| parse_projects(&value.value))
            .transpose()?
            .unwrap_or_default();
        let mut projects = self.projects.write().await;
        let removed: Vec<_> = projects
            .iter()
            .filter(|(id, project)| settings.get(*id) != Some(&project.settings))
            .map(|(id, _)| id.clone())
            .collect();
        for id in removed {
            if let Some(mut project) = projects.remove(&id) {
                project.service.retire().await;
                project.worker.abort();
                let _ = (&mut project.worker).await;
            }
            self.status.remove(&id);
        }
        for (id, settings) in settings {
            if projects.contains_key(&id) {
                continue;
            }
            let generation = Uuid::new_v4();
            let service = Arc::new(SlackService::for_project(
                Arc::clone(&self.notification),
                id.clone(),
                settings.clone(),
                generation,
                Arc::clone(&self.status),
                &self.backend,
                Arc::clone(&self.config),
            ));
            self.status
                .register(&id, generation, service.public_status().await);
            let worker = tokio::spawn(Arc::clone(&service).run());
            projects.insert(
                id,
                Project {
                    settings,
                    service,
                    worker,
                },
            );
        }
        Ok(())
    }
}

impl ConfigChangeHandler for Projects {
    fn record_creation_keys(&self) -> &'static [&'static str] {
        &["teams"]
    }

    fn prepare_record<'a>(
        &'a self,
        _key: &'a str,
        instance: &'a str,
        value: Value,
        config: std::sync::Weak<ConfigService>,
    ) -> ActionFuture<'a> {
        Box::pin(async move {
            if value["name"]
                .as_str()
                .is_none_or(|name| name.trim().is_empty())
            {
                return Err(ConfigServiceError::InvalidValue(
                    "请输入工作区显示名称。".into(),
                ));
            }
            let settings = parse_projects(&serde_json::json!([value]))
                .map_err(|message| Self::rejected("teams", message))?
                .remove(instance)
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let service = Arc::new(SlackService::for_project(
                Arc::clone(&self.notification),
                instance.to_owned(),
                settings,
                Uuid::new_v4(),
                Arc::clone(&self.status),
                &self.backend,
                Arc::clone(&self.config),
            ));
            self.wait_import(instance, service, config, Some(value))
                .await
        })
    }

    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.key == "teams" {
                parse_projects(&change.value)?;
            }
            Ok(())
        })
    }

    fn committed(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { self.sync_projects().await })
    }
}
