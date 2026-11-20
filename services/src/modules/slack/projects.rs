use std::{
    collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration,
};

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::any,
};
use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use tokio::{
    sync::{Mutex, Notify, RwLock, oneshot},
    task::JoinHandle,
};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    modules::{
        config::service::{
            ActionFuture, ChangeFuture, ConfigChange, ConfigChangeHandler, ConfigService,
            ConfigServiceError,
        },
        emacs::service::EmacsService,
        notification::NotificationService,
        password::{config::PasswordConfig, service::PasswordService},
        status::StatusService,
    },
    runtime::ServicePfile,
};

use super::{
    password::{SlackPasswords, validate_file},
    protocol::{Error, valid_id},
    routes,
    service::SlackService,
    status::ProjectsStatus,
    store::{Keychain, LoginKeychain, local_token},
};

pub(super) type BrowserFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

pub(super) struct Backend {
    pub path: Option<PathBuf>,
    pub api: String,
    pub client_id: Option<&'static str>,
    pub keychain: Arc<dyn Fn(String) -> Arc<dyn Keychain> + Send + Sync>,
    pub passwords: Option<Arc<SlackPasswords>>,
    pub emacs: Option<Arc<EmacsService>>,
    pub open_browser: Arc<dyn Fn(String) -> BrowserFuture + Send + Sync>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(super) struct SlackTeam {
    pub name: String,
    #[serde(default, rename = "id")]
    pub legacy_id: Option<String>,
    pub team_id: Option<String>,
    pub password_id: Option<String>,
}

pub(crate) fn valid_project(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
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
            .filter(|team| {
                team.starts_with('T') && team.len() > 1 && valid_id(team) && valid_project(team)
            })
            .ok_or("Slack Team ID 无效。")?
            .clone();
        if settings.name.trim().is_empty() || settings.name.chars().count() > 80 {
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
    router: Router,
    worker: JoinHandle<()>,
}

impl Drop for Project {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

struct TeamDraft {
    attempt: Uuid,
    record: Value,
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
    local_token: std::sync::OnceLock<String>,
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
        emacs: Arc<EmacsService>,
    ) -> Self {
        let passwords = Arc::new(SlackPasswords::new(
            PasswordConfig::new(config.clone()),
            passwords,
        ));
        Self::with_backend(
            config,
            notification,
            status,
            Backend {
                path: ServicePfile::path().ok(),
                api: "https://slack.com/api".into(),
                client_id: super::APP_CLIENT_ID.filter(|id| {
                    id.split_once('.').is_some_and(|(app, workspace)| {
                        !app.is_empty()
                            && !workspace.is_empty()
                            && app
                                .bytes()
                                .chain(workspace.bytes())
                                .all(|byte| byte.is_ascii_digit())
                    }) && id.len() <= 80
                }),
                keychain: Arc::new(|account| Arc::new(LoginKeychain(account))),
                passwords: Some(passwords),
                emacs: Some(emacs),
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
                            Err("无法打开 Slack 授权浏览器。".into())
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
            local_token: std::sync::OnceLock::new(),
            backend,
            shutdown: Notify::new(),
        }
    }

    pub fn router(self: &Arc<Self>) -> Router {
        let router = crate::http::mount_routes(Router::new(), "team-management");
        crate::http::mount_route_paths(router, &["team-authenticated", "team-oauth"], any(dispatch))
            .with_state(Arc::clone(self))
    }

    pub async fn run(self: Arc<Self>, port: u16) {
        let _ = self.port.set(port);
        if let Some(path) = &self.backend.path {
            let path = path.with_file_name("slack-api-token");
            if let Ok(Ok(token)) = tokio::task::spawn_blocking(move || local_token(&path)).await {
                let _ = self.local_token.set(token);
            }
        }
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

    async fn finish_draft(&self, id: &str, service: &Arc<SlackService>) {
        let mut drafts = self.drafts.lock().await;
        if drafts
            .get(id)
            .is_none_or(|draft| !Arc::ptr_eq(&draft.service, service))
        {
            return;
        }
        let draft = drafts.remove(id).expect("待授权工作区仍在初始请求中");
        let outcome = async {
            service
                .authorization_result(draft.attempt)
                .await
                .ok_or_else(|| Self::rejected("teams", "Slack 授权还未完成。"))?
                .map_err(|error| Self::slack_rejected("teams", error))?;
            let mut record = draft.record;
            let config = draft
                .config
                .upgrade()
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let reference = service
                .credential_id()
                .await
                .ok_or(ConfigServiceError::ActionNotFound)?;
            record["oauth_credential_id"] = Value::String(reference);
            config
                .replace_record(
                    "slack".into(),
                    "teams".into(),
                    id.into(),
                    record,
                    true,
                    true,
                )
                .await
        }
        .await;
        if outcome.is_err() {
            let session = service
                .state
                .lock()
                .await
                .secrets
                .account
                .as_ref()
                .map(|account| account.identity.session_id);
            if let Some(session) = session {
                let _ = service.disconnect(session).await;
            }
        }
        let _ = draft.outcome.send(outcome);
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

    async fn discard_draft(&self, id: &str, attempt: Uuid) -> bool {
        let mut drafts = self.drafts.lock().await;
        if drafts.get(id).is_some_and(|draft| draft.attempt == attempt) {
            if let Some(draft) = drafts.remove(id) {
                draft.service.retire().await;
            }
            true
        } else {
            false
        }
    }

    async fn sync_projects(&self) -> Result<(), String> {
        let _sync = self.reconcile.lock().await;
        let Some(port) = self.port.get().copied() else {
            return Ok(());
        };
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
            ));
            self.status
                .register(&id, generation, service.public_status().await);
            let worker = tokio::spawn(Arc::clone(&service).run(port));
            projects.insert(
                id,
                Project {
                    settings,
                    router: routes::router(Arc::clone(&service)),
                    service,
                    worker,
                },
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    project: String,
    token: String,
}

fn credential(value: Value) -> Result<Credential, String> {
    let input: Credential =
        serde_json::from_value(value).map_err(|_| "凭据需要项目标识和应用级令牌。".to_owned())?;
    if !valid_project(&input.project) || !SlackService::valid_app_token(&input.token) {
        return Err("请核对项目标识和 Slack 应用级令牌格式。".into());
    }
    Ok(input)
}

impl ConfigChangeHandler for Projects {
    fn prepare_record<'a>(
        &'a self,
        field: &'a str,
        instance: &'a str,
        value: Value,
        config: std::sync::Weak<ConfigService>,
    ) -> ActionFuture<'a> {
        Box::pin(async move {
            if field != "team_id" {
                return Err(ConfigServiceError::ActionNotFound);
            }
            let settings = parse_projects(&serde_json::json!([value]))
                .map_err(|message| Self::rejected("teams", message))?
                .remove(instance)
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let port = self
                .port
                .get()
                .copied()
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let (sender, mut receiver) = oneshot::channel();
            let (attempt, data) = {
                let mut drafts = self.drafts.lock().await;
                if drafts.contains_key(instance) {
                    return Err(Self::rejected("teams", "这个工作区已有待完成的授权。"));
                }
                let service = Arc::new(SlackService::for_project(
                    Arc::clone(&self.notification),
                    instance.to_owned(),
                    settings,
                    Uuid::new_v4(),
                    Arc::clone(&self.status),
                    &self.backend,
                ));
                service.set_port(port);
                let (attempt, data) = service
                    .begin_authorization()
                    .await
                    .map_err(|error| Self::slack_rejected("teams", error))?;
                drafts.insert(
                    instance.into(),
                    TeamDraft {
                        attempt,
                        record: value,
                        config,
                        service,
                        outcome: sender,
                    },
                );
                (attempt, data)
            };
            let url = data["authorization_url"]
                .as_str()
                .expect("Slack 授权结果包含地址");
            let failure = match (self.backend.open_browser)(url.to_owned()).await {
                Ok(()) => match tokio::time::timeout(Duration::from_secs(600), &mut receiver).await
                {
                    Ok(result) => return result.map_err(|_| ConfigServiceError::ActionNotFound)?,
                    Err(_) => "等待 Slack 授权超时。".into(),
                },
                Err(error) => error,
            };
            if self.discard_draft(instance, attempt).await {
                Err(Self::rejected("teams", failure))
            } else {
                receiver
                    .await
                    .map_err(|_| ConfigServiceError::ActionNotFound)?
            }
        })
    }

    fn action<'a>(&'a self, action: &'a str, instance: &'a str) -> ActionFuture<'a> {
        Box::pin(async move {
            let service = self
                .projects
                .read()
                .await
                .get(instance)
                .map(|project| Arc::clone(&project.service))
                .ok_or(ConfigServiceError::ActionNotFound)?;
            let rejected = |error| Self::slack_rejected(action, error);
            match action {
                "check_app_token" => {
                    service.check_app_token().await.map_err(rejected)?;
                    Ok(serde_json::json!({"valid":true,"message":"应用令牌可用。"}))
                }
                "authorize" => {
                    let (attempt, data) = service.begin_authorization().await.map_err(rejected)?;
                    let url = data["authorization_url"]
                        .as_str()
                        .expect("Slack 授权结果包含地址");
                    if let Err(error) = (self.backend.open_browser)(url.to_owned()).await {
                        service.cancel_authorization(attempt).await;
                        return Err(Self::rejected(action, error));
                    }
                    if let Err(error) = service.wait_authorization(attempt).await {
                        service.cancel_authorization(attempt).await;
                        return Err(rejected(error));
                    }
                    Ok(serde_json::json!({"message":"Slack 授权成功。"}))
                }
                _ => Err(ConfigServiceError::ActionNotFound),
            }
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

    fn secret_is_set<'a>(
        &'a self,
        _key: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async {
            let services: Vec<_> = self
                .projects
                .read()
                .await
                .values()
                .map(|project| Arc::clone(&project.service))
                .collect();
            for service in services {
                if service.state.lock().await.app_token.is_some() {
                    return true;
                }
            }
            false
        })
    }

    fn authorize_secret<'a>(&'a self, bearer: Option<&'a str>) -> ChangeFuture<'a> {
        Box::pin(async move {
            if self
                .local_token
                .get()
                .zip(bearer)
                .is_some_and(|(expected, provided)| {
                    bool::from(expected.as_bytes().ct_eq(provided.as_bytes()))
                })
            {
                Ok(())
            } else {
                Err("需要有效的本机凭据。".into())
            }
        })
    }

    fn validate_secret<'a>(&'a self, _key: &'a str, value: &'a Value) -> ChangeFuture<'a> {
        Box::pin(async move {
            let input = credential(value.clone())?;
            if self.projects.read().await.contains_key(&input.project) {
                Ok(())
            } else {
                Err("请先在项目配置中保存该项目。".into())
            }
        })
    }

    fn write_secret<'a>(&'a self, _key: &'a str, value: Value) -> ChangeFuture<'a> {
        Box::pin(async move {
            let input = credential(value)?;
            let service = self
                .projects
                .read()
                .await
                .get(&input.project)
                .map(|project| Arc::clone(&project.service))
                .ok_or("未找到 Slack 项目。")?;
            service.app_token(input.token).await.map_err(|error| {
                error.object()["message"]
                    .as_str()
                    .unwrap_or("Slack 凭据保存失败。")
                    .to_owned()
            })
        })
    }
}

#[loki_services_macros::loki_api(
    module = "slack",
    id = "open-team",
    method = "POST",
    path = "/slack/teams/{team_id}/open",
    mount = "Arc<Projects>",
    mount_group = "team-management",
    summary = "打开 Slack 工作区",
    description = "用 Emacs 打开本机已配置的工作区。"
)]
async fn open_team(
    State(projects): State<Arc<Projects>>,
    axum::extract::Path(team_id): axum::extract::Path<String>,
) -> Result<axum::Json<Value>, Error> {
    if !valid_project(&team_id) || !projects.projects.read().await.contains_key(&team_id) {
        return Err(Error::Project);
    }
    let emacs = projects.backend.emacs.as_ref().ok_or(Error::Unavailable)?;
    // 工作区标识只允许字母、数字、连字符和下划线。
    let script = format!(
        r#"(progn
 (require 'seq)
 (when (require 'loki-assistant-slack nil t)
   (let ((frame (or (and (display-graphic-p (selected-frame)) (selected-frame))
                    (seq-find #'display-graphic-p (frame-list))
                    (make-frame '((window-system . ns))))))
     (select-frame frame)
     (loki-assistant-slack "{team_id}")
     (make-frame-visible frame)
     (select-frame-set-input-focus frame)
     t)))"#
    );
    match emacs.eval(&script).await {
        Ok(value) if value == "t" => Ok(axum::Json(
            serde_json::json!({"api_version":"1","opened":true}),
        )),
        _ => Err(Error::Unavailable),
    }
}

async fn dispatch(State(projects): State<Arc<Projects>>, request: Request) -> Response {
    let uri = request.uri();
    let callback = uri.path() == "/slack/oauth/callback";
    let callback_project = callback
        .then(|| {
            url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                .find(|(key, _)| key == "state")
                .and_then(|(_, value)| value.split_once('.').map(|(id, _)| id.to_owned()))
        })
        .flatten();
    if let Some(id) = callback_project.as_deref() {
        let candidate = projects
            .drafts
            .lock()
            .await
            .get(id)
            .map(|draft| Arc::clone(&draft.service));
        if let Some(service) = candidate {
            // OAuth code 是一次性的。浏览器关闭时也需要完成已收到的回调。
            let id = id.to_owned();
            return tokio::spawn(async move {
                let response = routes::router(Arc::clone(&service))
                    .oneshot(request)
                    .await
                    .expect("Axum 路由不可失败");
                if response.status().is_redirection() {
                    projects.finish_draft(&id, &service).await;
                }
                response
            })
            .await
            .unwrap_or_else(|_| Error::Unavailable.into_response());
        }
    }
    if uri.path() == "/slack/oauth/result" {
        return axum::response::Html("<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>Slack 授权</title><p>请返回配置界面查看 Slack 授权结果。</p></html>").into_response();
    }
    let assigned = request.headers().get("x-loki-slack-project");
    let active = projects.projects.read().await;
    let project = if uri.path() == "/slack/oauth/result" {
        active.values().next()
    } else if callback {
        callback_project.as_deref().and_then(|id| active.get(id))
    } else if let Some(assigned) = assigned {
        assigned.to_str().ok().and_then(|id| active.get(id))
    } else {
        active.get("default").or_else(|| {
            if active.len() == 1 {
                active.values().next()
            } else {
                None
            }
        })
    };
    let router = project.map(|project| project.router.clone());
    drop(active);
    if let Some(router) = router {
        return router.oneshot(request).await.expect("Axum 路由不可失败");
    }
    let mut response = if callback {
        Error::OAuthState
    } else {
        Error::Project
    }
    .into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
