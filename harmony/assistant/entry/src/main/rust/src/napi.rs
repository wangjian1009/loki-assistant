use std::{future::Future, sync::Arc, time::Duration};

use napi_derive_ohos::napi;
use napi_ohos::{
    Env, Error, Result, Status,
    bindgen_prelude::{FnArgs, Function, Object, spawn},
    threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{
    Workspace as RustWorkspace, WorkspaceOptions as RustWorkspaceOptions,
    agent_conversations::{ConversationObserver, ConversationSubscriptions},
    agent_shell::{AgentShell as RustAgentShell, AgentShellSession},
    agent_shell_images::ImageCache,
    mail::{Mail as RustMail, MailStatusChanged},
    network::{
        InvalidRequestReason, Network as RustNetwork, QueryParameter,
        RequestHandle as RustRequestHandle, RequestOptions as RustRequestOptions, ResponsError,
        ServiceRequest as RustServiceRequest,
    },
    service_proxy::{DeviceRefreshEvent, StatusSubscription as RustStatusSubscription},
    slack::{Slack as RustSlack, SlackStatusChanged},
    workspace::ServiceConfiguration,
};

fn js_value(value: impl Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Error::from_reason(error.to_string()))
}

fn forward_request<T: Serialize + Send + 'static>(
    request: impl Future<Output = std::result::Result<T, ResponsError>> + Send + 'static,
    on_success: Function<'_, Value, ()>,
    on_error: Function<'_, Value, ()>,
) -> Result<()> {
    let on_success = on_success
        .build_threadsafe_function()
        .callee_handled::<false>()
        .weak::<true>()
        .build()?;
    let on_error = on_error
        .build_threadsafe_function()
        .callee_handled::<false>()
        .weak::<true>()
        .build()?;
    spawn(async move {
        match request.await {
            Ok(data) => {
                if let Ok(data) = js_value(data) {
                    on_success.call(data, NonBlocking);
                }
            }
            Err(error) => {
                if let Ok(error) = js_value(error) {
                    on_error.call(error, NonBlocking);
                }
            }
        }
    });
    Ok(())
}

#[napi]
pub fn render_document(source: String, format: String) -> String {
    match format.as_str() {
        "org" => loki_org_parser::org(&source),
        "markdown" => loki_org_parser::markdown(&source),
        _ => loki_org_parser::render(&source),
    }
}

fn typed_result<T>(env: Env, result: std::result::Result<T, ResponsError>) -> Result<T> {
    result.map_err(
        |error| match js_value(error).and_then(|value| env.throw(value)) {
            Ok(()) => Error::new(Status::PendingException, ""),
            Err(error) => error,
        },
    )
}

async fn blocking<T: Send + 'static>(
    action: impl FnOnce() -> std::result::Result<T, String> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|error| Error::from_reason(error.to_string()))?
        .map_err(Error::from_reason)
}

#[napi(object)]
pub struct WorkspaceOptions {
    pub directory: String,
}

#[napi]
pub struct Workspace {
    inner: Arc<RustWorkspace>,
    agent_images: Arc<ImageCache>,
}

#[napi]
impl Workspace {
    #[napi(factory)]
    pub async fn open(options: WorkspaceOptions) -> Result<Self> {
        blocking(move || RustWorkspace::open(RustWorkspaceOptions::new(options.directory)))
            .await
            .map(|inner| Self {
                inner: Arc::new(inner),
                agent_images: Arc::new(ImageCache::default()),
            })
    }

    #[napi]
    pub fn network(&self) -> Network {
        Network {
            inner: self.inner.network(),
        }
    }

    #[napi]
    pub fn open_mail(&self) -> Mail {
        Mail {
            inner: self.inner.open_mail(),
        }
    }

    #[napi]
    pub fn open_slack(&self) -> Slack {
        Slack {
            inner: self.inner.open_slack(),
        }
    }

    #[napi]
    pub fn open_agent_shell(&self) -> AgentShell {
        AgentShell {
            inner: Arc::new(self.inner.open_agent_shell()),
            images: self.agent_images.clone(),
            conversations: self.inner.open_agent_conversations(),
        }
    }

    #[napi(ts_return_type = "ApplicationData")]
    pub fn load_state(&self) -> Result<Value> {
        js_value(self.inner.load_state())
    }

    #[napi(ts_args_type = "state: Object")]
    pub async fn save_state(&self, state: Map<String, Value>) -> Result<()> {
        let workspace = self.inner.clone();
        blocking(move || workspace.save_state(state)).await
    }

    #[napi]
    pub fn refresh_device(&self, env: Env, endpoint: String) -> Result<()> {
        typed_result(env, self.inner.refresh_device(&endpoint))
    }

    #[napi(ts_args_type = "changed: (event: DeviceRefreshEvent) => void")]
    pub fn watch_refresh(&self, changed: Function<'_, Value, ()>) -> Result<StatusSubscription> {
        subscribe::<DeviceRefreshEvent>(changed, |changed| self.inner.watch_refresh(changed))
    }

    #[napi(ts_args_type = "changed: (snapshot: StatusSnapshot) => void")]
    pub fn watch_status(&self, changed: Function<'_, Value, ()>) -> Result<StatusSubscription> {
        subscribe(changed, |changed| self.inner.watch_status(changed))
    }

    #[napi(ts_args_type = "path: string, methods: string[], dispatch: HttpDispatch")]
    pub fn register_http_route(
        &self,
        path: String,
        methods: Vec<String>,
        dispatch: Function<'_, FnArgs<(String, String, String)>, ()>,
    ) -> Result<()> {
        let dispatch = dispatch
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;
        self.inner
            .register_http_route(
                &path,
                &methods,
                Box::new(move |id, method, body| {
                    dispatch.call(
                        (id.to_string(), method.to_owned(), body.to_owned()).into(),
                        NonBlocking,
                    ) == Status::Ok
                }),
            )
            .map_err(Error::from_reason)
    }

    #[napi]
    pub fn start_http_server(&self, port: u16) -> Result<String> {
        self.inner
            .start_http_server(port)
            .map(|address| address.to_string())
            .map_err(Error::from_reason)
    }

    #[napi]
    pub fn respond_http(&self, id: String, status: u16, body: String) -> Result<()> {
        let id = id
            .parse()
            .map_err(|_| Error::from_reason("调试 HTTP 请求编号无效"))?;
        self.inner.respond_http(id, status, &body);
        Ok(())
    }

    #[napi]
    pub async fn stop_http_server(&self) {
        self.inner.stop_http_server().await;
    }

    #[napi(ts_args_type = "configuration: ServiceConfiguration")]
    pub fn set_service(&self, env: Env, configuration: Value) -> Result<()> {
        let configuration: std::result::Result<ServiceConfiguration, _> =
            serde_json::from_value(configuration)
                .map_err(|_| InvalidRequestReason::InvalidService.into());
        typed_result(
            env,
            configuration.and_then(|configuration| self.inner.set_service(configuration)),
        )
    }

    #[napi]
    pub fn clear_service(&self) {
        self.inner.clear_service();
    }

    #[napi(ts_return_type = "ServiceState | null")]
    pub fn service(&self) -> Result<Value> {
        js_value(self.inner.service())
    }

    #[napi(ts_return_type = "Invitation")]
    pub fn parse_invitation(&self, env: Env, payload: String) -> Result<Value> {
        js_value(typed_result(env, self.inner.parse_invitation(&payload))?)
    }

    #[napi]
    pub async fn shutdown(&self) -> Result<()> {
        let workspace = self.inner.clone();
        workspace.begin_stop();
        blocking(move || {
            workspace.shutdown();
            Ok(())
        })
        .await
    }
}

#[napi]
pub struct Network {
    inner: Arc<RustNetwork>,
}

#[napi]
impl Network {
    #[napi(
        ts_args_type = "request: ServiceRequest, options: RequestOptions, onSuccess: (response: ServiceResponse) => void, onError: (error: ResponsError) => void"
    )]
    pub fn request(
        &self,
        request: Object<'_>,
        options: RequestOptions,
        on_success: Function<'_, Value, ()>,
        on_error: Function<'_, Value, ()>,
    ) -> Result<RequestHandle> {
        let on_success = on_success
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;
        let on_error = on_error
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;
        let Some(request) = parse_request(request)? else {
            on_error.call(
                js_value(ResponsError::from(InvalidRequestReason::InvalidMethod))?,
                NonBlocking,
            );
            return Ok(RequestHandle { inner: None });
        };
        let handle = self.inner.request::<Value, _, _>(
            request,
            RustRequestOptions {
                timeout: Duration::from_millis(u64::from(options.timeout_ms)),
            },
            move |response| {
                let response = serde_json::json!({
                    "request_id": response.request_id,
                    "status": response.status.as_u16(),
                    "data": response.data,
                    "generation": response.generation
                });
                on_success.call(response, NonBlocking);
            },
            move |error| {
                if let Ok(error) = js_value(error) {
                    on_error.call(error, NonBlocking);
                }
            },
        );
        Ok(RequestHandle {
            inner: Some(handle),
        })
    }
}

fn parse_request(request: Object<'_>) -> Result<Option<RustServiceRequest>> {
    let method = request
        .get::<String>("method")?
        .and_then(|method| method.parse().ok());
    let Some(method) = method else {
        return Ok(None);
    };
    let path: String = request
        .get("path")?
        .ok_or_else(|| Error::from_reason("请求路径无效"))?;
    let query: Vec<Query> = request.get("query")?.unwrap_or_default();
    Ok(Some(RustServiceRequest {
        method,
        path,
        query: query
            .into_iter()
            .map(|item| QueryParameter {
                name: item.name,
                value: item.value,
            })
            .collect(),
        body: request.get("body")?,
    }))
}

#[napi(object, js_name = "QueryParameter")]
pub struct Query {
    pub name: String,
    pub value: String,
}

#[napi(object)]
pub struct RequestOptions {
    #[napi(js_name = "timeout_ms")]
    pub timeout_ms: u32,
}

#[napi]
pub struct RequestHandle {
    inner: Option<RustRequestHandle>,
}

#[napi]
impl RequestHandle {
    #[napi]
    pub fn cancel(&self) {
        if let Some(handle) = &self.inner {
            handle.cancel();
        }
    }
}

#[napi]
pub struct Mail {
    inner: RustMail,
}

#[napi]
impl Mail {
    #[napi]
    pub fn start_sync(&self, env: Env, endpoint: String) -> Result<()> {
        typed_result(env, self.inner.start_sync(&endpoint))
    }

    #[napi(ts_args_type = "changed: (snapshot: MailStatusChanged) => void")]
    pub fn watch_status(&self, changed: Function<'_, Value, ()>) -> Result<StatusSubscription> {
        subscribe::<MailStatusChanged>(changed, |changed| self.inner.watch_status(changed))
    }
}

#[napi]
pub struct Slack {
    inner: RustSlack,
}

#[napi]
impl Slack {
    #[napi(ts_args_type = "changed: (snapshot: SlackStatusChanged) => void")]
    pub fn watch_status(&self, changed: Function<'_, Value, ()>) -> Result<StatusSubscription> {
        subscribe::<SlackStatusChanged>(changed, |changed| self.inner.watch_status(changed))
    }
}

#[napi]
pub struct AgentShell {
    inner: Arc<RustAgentShell>,
    images: Arc<ImageCache>,
    conversations: Arc<ConversationSubscriptions>,
}

#[napi]
impl AgentShell {
    #[napi(ts_return_type = "Promise<AgentShellAgent[]>")]
    pub async fn list_agents(&self, endpoint_id: String) -> Result<Value> {
        let agents = self
            .inner
            .list_agents(&endpoint_id)
            .await
            .map_err(|error| Error::from_reason(error.to_string()))?;
        js_value(agents)
    }

    #[napi(
        ts_args_type = "endpointId: string, onSuccess: (projects: AgentShellProject[]) => void, onError: (error: ResponsError) => void"
    )]
    pub fn list_projects(
        &self,
        endpoint_id: String,
        on_success: Function<'_, Value, ()>,
        on_error: Function<'_, Value, ()>,
    ) -> Result<()> {
        let shell = self.inner.clone();
        forward_request(
            async move { shell.list_projects(&endpoint_id).await },
            on_success,
            on_error,
        )
    }

    #[napi(
        ts_args_type = "endpointId: string, projectId: string, agentId: string, resumeSessionId: string | null, onSuccess: (sessionId: string) => void, onError: (error: ResponsError) => void"
    )]
    pub fn create_session(
        &self,
        endpoint_id: String,
        project_id: String,
        agent_id: String,
        resume_session_id: Option<String>,
        on_success: Function<'_, Value, ()>,
        on_error: Function<'_, Value, ()>,
    ) -> Result<()> {
        let shell = self.inner.clone();
        forward_request(
            async move {
                shell
                    .create_session(
                        &endpoint_id,
                        &project_id,
                        &agent_id,
                        resume_session_id.as_deref(),
                    )
                    .await
            },
            on_success,
            on_error,
        )
    }

    #[napi(
        ts_args_type = "endpointId: string, projectId: string, agentId: string, onSuccess: (sessions: AgentShellHistorySession[]) => void, onError: (error: ResponsError) => void"
    )]
    pub fn list_history(
        &self,
        endpoint_id: String,
        project_id: String,
        agent_id: String,
        on_success: Function<'_, Value, ()>,
        on_error: Function<'_, Value, ()>,
    ) -> Result<()> {
        let shell = self.inner.clone();
        forward_request(
            async move {
                shell
                    .list_history(&endpoint_id, &project_id, &agent_id)
                    .await
            },
            on_success,
            on_error,
        )
    }

    #[napi(ts_args_type = "changed: (sessions: AgentShellSession[] | null) => void")]
    pub fn watch_status(&self, changed: Function<'_, Value, ()>) -> Result<StatusSubscription> {
        subscribe::<Option<Vec<AgentShellSession>>>(changed, |changed| {
            self.inner.watch_status(changed)
        })
    }

    #[napi(ts_args_type = "sessionId: string, changed: (view: AgentShellConversationView) => void")]
    pub fn observe_conversation(
        &self,
        env: Env,
        session_id: String,
        changed: Function<'_, Value, ()>,
    ) -> Result<ConversationSubscription> {
        let changed = changed
            .build_threadsafe_function()
            .callee_handled::<false>()
            .weak::<true>()
            .build()?;
        let observer = self.conversations.observe(session_id, move |view| {
            js_value(view).is_ok_and(|value| changed.call(value, NonBlocking) == Status::Ok)
        });
        Ok(ConversationSubscription {
            inner: typed_result(env, observer)?,
        })
    }

    #[napi(ts_return_type = "Promise<AgentShellBlock[]>")]
    pub async fn preview_conversation(&self, session_id: String) -> Result<Value> {
        let blocks = self
            .conversations
            .preview(session_id)
            .await
            .map_err(|_| Error::from_reason("无法读取相邻会话"))?;
        js_value(blocks)
    }

    #[napi(ts_return_type = "Promise<ResponsError | null>")]
    pub async fn send_message(&self, session_id: String, text: String) -> Result<Value> {
        js_value(self.inner.send_message(&session_id, text).await.err())
    }

    #[napi(ts_return_type = "Promise<ResponsError | null>")]
    pub async fn restart_session(&self, session_id: String) -> Result<Value> {
        js_value(self.inner.restart_session(&session_id).await.err())
    }

    #[napi(ts_return_type = "Promise<ResponsError | null>")]
    pub async fn clear_buffer(&self, session_id: String) -> Result<Value> {
        js_value(self.inner.clear_buffer(&session_id).await.err())
    }

    #[napi]
    pub async fn cache_image(
        &self,
        cache_dir: String,
        session_id: String,
        source: String,
    ) -> Result<String> {
        self.images
            .clone()
            .read(&self.inner, cache_dir.into(), session_id, source)
            .await
            .map_err(Error::from_reason)
    }
    #[napi]
    pub async fn clear_session_images(
        &self,
        cache_dir: String,
        endpoint_id: String,
        session_id: String,
    ) {
        self.images
            .close(std::path::Path::new(&cache_dir), &endpoint_id, &session_id);
    }
}

#[napi]
pub struct ConversationSubscription {
    inner: ConversationObserver,
}

#[napi]
impl ConversationSubscription {
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

#[napi]
pub struct StatusSubscription {
    inner: RustStatusSubscription,
}

#[napi]
impl StatusSubscription {
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

fn subscribe<T: Serialize + 'static>(
    changed: Function<'_, Value, ()>,
    listen: impl FnOnce(Box<dyn FnMut(T) -> bool + Send>) -> RustStatusSubscription,
) -> Result<StatusSubscription> {
    let changed = changed
        .build_threadsafe_function()
        .callee_handled::<false>()
        .weak::<true>()
        .build()?;
    Ok(StatusSubscription {
        inner: listen(Box::new(move |update| {
            js_value(update).is_ok_and(|value| changed.call(value, NonBlocking) == Status::Ok)
        })),
    })
}
