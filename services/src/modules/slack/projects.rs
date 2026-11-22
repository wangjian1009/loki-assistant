use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use axum::Router;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    sync::{Mutex, Notify, RwLock},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::modules::{
    config::service::{ChangeFuture, ConfigChange, ConfigChangeHandler, ConfigService},
    notification::NotificationService,
    status::StatusService,
};

use super::{
    mailbox::{next_cursor, read_cursor},
    protocol::{Error, Page, valid_id},
    routes,
    service::SlackService,
    status::ProjectsStatus,
};

pub(super) type BrowserFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub(super) enum RegistrationStatus {
    NeedsCredentials,
    Pending,
    Confirmed(Option<SlackTeamIdentity>),
    Invalid(Error),
}

pub(super) struct Backend {
    pub api: String,
    pub open_browser: Arc<dyn Fn(String) -> BrowserFuture + Send + Sync>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(super) struct SlackTeam {
    #[serde(default)]
    pub name: String,
    pub team_id: Option<String>,
    pub token: Option<String>,
    pub d_cookie: Option<String>,
    #[serde(flatten)]
    pub identity: SlackTeamIdentity,
}

#[derive(Clone, Debug, Default, Deserialize, serde::Serialize, Eq, PartialEq)]
pub(super) struct SlackTeamIdentity {
    pub team_name: Option<String>,
    pub user_id: Option<String>,
}

impl SlackTeam {
    pub(super) const PASSWORD_FIELDS: [&str; 2] = ["token", "d_cookie"];
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
        projects.insert(id, settings);
    }
    Ok(projects)
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

pub struct Projects {
    config: Arc<ConfigService>,
    notification: Arc<NotificationService>,
    pub(super) status: Arc<ProjectsStatus>,
    projects: RwLock<BTreeMap<String, Project>>,
    reconcile: Mutex<()>,
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
    ) -> Self {
        Self::with_backend(
            config,
            notification,
            status,
            Backend {
                api: "https://slack.com/api".into(),
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
        let _ = self.sync_projects(None).await;
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

    pub(super) async fn open_team(&self, team: &str) -> Result<(), String> {
        let port = self.port.get().ok_or("Slack 模块尚未启动。")?;
        (self.backend.open_browser)(format!("http://127.0.0.1:{port}/slack/team-setup/{team}"))
            .await
    }

    pub(super) async fn registration_status(&self, id: Uuid, team: &str) -> RegistrationStatus {
        let passwords = match self
            .config
            .notification_passwords(id, "slack", "teams", team, SlackTeam::PASSWORD_FIELDS)
            .await
        {
            Ok(None) => return RegistrationStatus::Confirmed(None),
            Ok(Some(passwords)) => passwords,
            Err(_) => return RegistrationStatus::Invalid(Error::PasswordRead),
        };
        let session = match super::client::WebSession::from_passwords(&passwords) {
            Ok(Some(session)) => session,
            Ok(None) => return RegistrationStatus::NeedsCredentials,
            Err(error) => return RegistrationStatus::Invalid(error),
        };
        match super::client::SlackClient::new(&self.backend.api)
            .web_call("auth.test", &session, &[], None)
            .await
        {
            Ok(identity) if identity["team_id"] != team => {
                RegistrationStatus::Invalid(Error::TeamMismatch)
            }
            Ok(identity) if !identity["bot_id"].is_string() => {
                let Some(user_id) = identity["user_id"].as_str().filter(|id| valid_id(id)) else {
                    return RegistrationStatus::Invalid(Error::WebUserToken);
                };
                let team_name = identity["team"]
                    .as_str()
                    .map(str::trim)
                    .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
                    .unwrap_or(team);
                RegistrationStatus::Confirmed(Some(SlackTeamIdentity {
                    team_name: Some(team_name.to_owned()),
                    user_id: Some(user_id.to_owned()),
                }))
            }
            Ok(_) => RegistrationStatus::Invalid(Error::WebUserToken),
            Err(Error::Unavailable | Error::RateLimited(_)) => RegistrationStatus::Pending,
            Err(error) => RegistrationStatus::Invalid(error),
        }
    }

    pub(super) async fn sync_projects(&self, reload_team: Option<&str>) -> Result<(), String> {
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
            .filter(|(id, project)| {
                Some(id.as_str()) == reload_team || settings.get(*id) != Some(&project.settings)
            })
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
    fn handle(&self, change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async move {
            if change.key == "teams" {
                parse_projects(&change.value)?;
            }
            Ok(())
        })
    }

    fn committed(&self, _change: ConfigChange) -> ChangeFuture<'_> {
        Box::pin(async { self.sync_projects(None).await })
    }
}
