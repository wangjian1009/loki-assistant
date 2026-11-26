use std::{
    collections::BTreeMap,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{Stream, StreamExt, stream};
use serde::Deserialize;
use serde_json::Value;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::{
    archive::{self, Archive, Update, Writer},
    config::{
        self, CLIENTS, Filter, MODULE, MonitoringFilters, PROVIDER_TOKEN, PROVIDERS, provider_uri,
    },
    instructions::Format,
    monitor::{Monitor, RequestGuard},
    protocol::{Binding, Client, IssuedClient, Phase, Protocol, error},
    usage::{RequestedModel, UsageReader},
};
use crate::{
    modules::config::service::{ConfigService, ConfigServiceError},
    process::Process,
};

const TOKEN_PREFIX: &str = "loki-llm-";
const API_KEY: &str = "x-api-key";

#[derive(Clone, Deserialize)]
struct StoredClient {
    name: String,
    token: String,
    #[serde(flatten)]
    binding: Binding,
}

type Clients = BTreeMap<Uuid, StoredClient>;

#[derive(Default)]
struct Authorization {
    clients: Clients,
    providers: BTreeMap<String, Provider>,
}

struct Provider {
    codex: Result<Option<Upstream>, &'static str>,
    claude: Result<Option<Upstream>, &'static str>,
}

#[derive(Clone)]
struct Upstream {
    uri: url::Url,
    credential: Option<HeaderValue>,
}

fn token_id(token: &str) -> Option<Uuid> {
    let (id, secret) = token.strip_prefix(TOKEN_PREFIX)?.split_once('.')?;
    (secret.len() == 43).then_some(())?;
    Uuid::parse_str(id).ok()
}

pub(super) fn new_token(id: Uuid) -> Result<String, getrandom::Error> {
    let mut secret = [0; 32];
    getrandom::fill(&mut secret)?;
    Ok(format!(
        "{TOKEN_PREFIX}{id}.{}",
        URL_SAFE_NO_PAD.encode(secret)
    ))
}

#[derive(Debug)]
pub(crate) struct LlmService {
    config: Arc<ConfigService>,
    authorization: Process<Authorization>,
    pub(crate) monitor: Monitor,
    pub(super) archive: Arc<Archive>,
    http: reqwest::Client,
}

impl LlmService {
    pub(crate) fn new(
        config: Arc<ConfigService>,
        monitor: Monitor,
        archive_directory: Option<PathBuf>,
    ) -> Self {
        let archive = Archive::new(archive_directory);
        archive.schedule(config.clone());
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            config,
            authorization: Process::start(Authorization::default()),
            monitor,
            archive,
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .read_timeout(Duration::from_secs(300))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .build()
                .expect("应能创建 LLM HTTP 客户端"),
        }
    }

    pub(crate) fn router(self: &Arc<Self>) -> axum::Router {
        crate::http::mount_routes(axum::Router::new(), MODULE).with_state(self.clone())
    }

    pub(crate) async fn reload_authorization(&self) -> Result<(), ConfigServiceError> {
        let config = self.config.clone();
        self.authorization
            .with(move |state| {
                Box::pin(async move {
                    let values = config.module_values(MODULE).await?;
                    let mut clients = Clients::new();
                    let records: Vec<StoredClient> =
                        serde_json::from_value(values[CLIENTS].value.clone()).map_err(|_| {
                            ConfigServiceError::InvalidValue("LLM 客户端配置格式无效。".into())
                        })?;
                    for record in records {
                        let id = token_id(&record.token).ok_or_else(|| {
                            ConfigServiceError::InvalidValue("LLM 客户端令牌格式无效。".into())
                        })?;
                        if clients.insert(id, record).is_some() {
                            return Err(ConfigServiceError::InvalidValue(
                                "LLM 客户端令牌重复。".into(),
                            ));
                        }
                    }
                    let mut providers = BTreeMap::new();
                    for record in values[PROVIDERS]
                        .value
                        .as_array()
                        .expect("供应商配置应为列表")
                    {
                        let id = record["id"].as_str().expect("供应商记录应有标识");
                        let token = read_provider_token(&config, id, record).await;
                        let codex = read_upstream(record, Protocol::Codex, &token);
                        let claude = read_upstream(record, Protocol::Claude, &token);
                        providers.insert(id.to_owned(), Provider { codex, claude });
                    }
                    *state = Authorization { clients, providers };
                    Ok(())
                })
            })
            .await
    }

    pub(super) async fn prepare_client(
        &self,
        upstream: String,
        id: Option<Uuid>,
    ) -> Result<Option<String>, String> {
        self.authorization
            .call(move |state| {
                if !state.providers.contains_key(&upstream) {
                    return Err("供应商不存在。".into());
                }
                id.map(new_token)
                    .transpose()
                    .map_err(|_| "无法生成客户端令牌。".into())
            })
            .await
    }

    pub(super) async fn provider_ids(&self) -> Vec<String> {
        self.authorization
            .call(|state| state.providers.keys().cloned().collect())
            .await
    }

    pub(super) async fn clients(&self) -> Vec<Client> {
        self.authorization
            .call(|state| {
                state
                    .clients
                    .iter()
                    .map(|(&id, record)| Client {
                        id,
                        binding: record.binding.clone(),
                    })
                    .collect()
            })
            .await
    }

    pub(super) async fn client_names(&self) -> BTreeMap<String, String> {
        self.authorization
            .call(|state| {
                state
                    .clients
                    .iter()
                    .map(|(id, client)| (id.to_string(), client.name.clone()))
                    .collect()
            })
            .await
    }

    pub(super) async fn issue(
        &self,
        name: Option<String>,
        binding: Binding,
    ) -> Result<IssuedClient, Response> {
        let name = name.unwrap_or_else(|| Uuid::new_v4().to_string());
        let (value, _) = self
            .config
            .notify_write(
                MODULE.into(),
                CLIENTS.into(),
                Some(name),
                Some(serde_json::json!({"upstream": binding.upstream})),
                true,
            )
            .await
            .map_err(config_change_error)?
            .ok_or_else(config_error)?;
        let token = value["token"].as_str().ok_or_else(config_error)?.to_owned();
        let id = token_id(&token).ok_or_else(config_error)?;
        Ok(IssuedClient {
            client: Client { id, binding },
            token,
        })
    }

    pub(super) async fn switch(&self, id: Uuid, binding: Binding) -> Result<Client, Response> {
        let name = self.client_name(id).await?;
        self.config
            .notify_patch(
                MODULE.into(),
                CLIENTS.into(),
                Some(name),
                serde_json::json!({"upstream": binding.upstream}),
            )
            .await
            .map_err(config_change_error)?;
        Ok(Client { id, binding })
    }

    pub(super) async fn revoke(&self, id: Uuid) -> Result<(), Response> {
        let name = self.client_name(id).await?;
        self.config
            .notify_write(MODULE.into(), CLIENTS.into(), Some(name), None, false)
            .await
            .map_err(config_change_error)?;
        Ok(())
    }

    async fn client_name(&self, id: Uuid) -> Result<String, Response> {
        self.authorization
            .call(move |state| state.clients.get(&id).map(|client| client.name.clone()))
            .await
            .ok_or_else(|| {
                error(
                    StatusCode::NOT_FOUND,
                    "llm-client-not-found",
                    "客户端不存在。",
                )
            })
    }

    async fn authenticate(
        &self,
        headers: &HeaderMap,
        protocol: Protocol,
    ) -> Result<(Client, Upstream), Response> {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .filter(|(kind, _)| kind.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token);
        let token = bearer
            .or_else(|| {
                (protocol == Protocol::Claude)
                    .then(|| headers.get(API_KEY)?.to_str().ok())
                    .flatten()
            })
            .ok_or_else(invalid_token)?
            .to_owned();
        self.authorization
            .call(move |state| {
                let id = token_id(&token).ok_or_else(invalid_token)?;
                let record = state
                    .clients
                    .get(&id)
                    .filter(|record| bool::from(token.as_bytes().ct_eq(record.token.as_bytes())))
                    .ok_or_else(invalid_token)?;
                let provider = state
                    .providers
                    .get(&record.binding.upstream)
                    .ok_or_else(|| {
                        error(
                            StatusCode::BAD_GATEWAY,
                            "llm-provider-not-found",
                            "当前绑定的供应商不存在。",
                        )
                    })?;
                let upstream = match protocol {
                    Protocol::Codex => &provider.codex,
                    Protocol::Claude => &provider.claude,
                }
                .as_ref()
                .map_err(|code| error(StatusCode::BAD_GATEWAY, code, "供应商的 Token 不可用。"))?
                .clone()
                .ok_or_else(|| {
                    error(
                        StatusCode::BAD_GATEWAY,
                        "llm-protocol-unavailable",
                        "当前供应商未配置此协议。",
                    )
                })?;
                Ok((
                    Client {
                        id,
                        binding: record.binding.clone(),
                    },
                    upstream,
                ))
            })
            .await
    }

    pub(super) async fn monitoring(&self) -> Result<MonitoringFilters, ConfigServiceError> {
        config::monitoring(&self.config).await
    }

    pub(super) async fn change_monitoring(
        &self,
        filter: Filter,
    ) -> Result<MonitoringFilters, ConfigServiceError> {
        config::set_monitoring_filter(&self.config, filter).await
    }

    pub(super) async fn forward(
        &self,
        protocol: Protocol,
        request: Request,
    ) -> Result<Response, Response> {
        let (
            client,
            Upstream {
                mut uri,
                credential,
            },
        ) = self.authenticate(request.headers(), protocol).await?;
        let (parts, body) = request.into_parts();
        let endpoint = parts
            .uri
            .path()
            .strip_prefix(protocol.prefix())
            .expect("代理路由应包含协议前缀");
        uri.set_path(&format!("{}{endpoint}", uri.path().trim_end_matches('/')));
        uri.set_query(parts.uri.query());

        let mut headers = forwarded_headers(&parts.headers);
        headers.remove(header::AUTHORIZATION);
        headers.remove(API_KEY);
        headers.remove(header::HOST);
        if let Some(credential) = credential {
            let header = match protocol {
                Protocol::Codex => header::AUTHORIZATION,
                Protocol::Claude => HeaderName::from_static(API_KEY),
            };
            headers.insert(header, credential);
        }

        let head = parts.method == Method::HEAD;
        let track_usage = UsageReader::tracks(&parts.method, endpoint);
        let started = Instant::now();
        let (mut guard, request) =
            self.monitor
                .start(client, protocol, &parts.headers, track_usage);
        let writer = if track_usage && let Some(format) = Format::for_endpoint(protocol, endpoint) {
            match config::monitoring_filter(&self.config).await {
                Ok(filter) => match self
                    .archive
                    .begin(&filter, &request, format, identity_encoding(&parts.headers))
                    .await
                {
                    Ok(writer) => writer,
                    Err(err) => {
                        log::warn!("打开 LLM 请求缓存失败：{err}");
                        None
                    }
                },
                Err(err) => {
                    log::warn!("无法读取 LLM 监控配置：{err}");
                    None
                }
            }
        } else {
            None
        };
        let mut upstream = self.http.request(parts.method, uri).headers(headers);
        if !body.is_end_stream() {
            let body = body.into_data_stream();
            let model = (track_usage && identity_encoding(&parts.headers))
                .then(|| (RequestedModel::default(), guard.request_model_callback()));
            let body = if model.is_some() || writer.is_some() {
                reqwest::Body::wrap_stream(stream::unfold(
                    (body, writer.clone(), model),
                    |(mut body, writer, mut model)| async move {
                        let item = body.next().await?;
                        if let Ok(chunk) = &item {
                            if let Some((mut reader, found)) = model.take() {
                                if let Some(value) = reader.push(chunk) {
                                    found(value);
                                } else {
                                    model = Some((reader, found));
                                }
                            }
                            if let Some(writer) = &writer {
                                writer.request(chunk).await;
                            }
                        }
                        Some((item, (body, writer, model)))
                    },
                ))
            } else {
                reqwest::Body::wrap_stream(body)
            };
            upstream = upstream.body(body);
        }
        let upstream = match upstream.send().await {
            Ok(response) => response,
            Err(error_value) => {
                if let Some(writer) = &writer {
                    writer
                        .update(archive::finished(Phase::Failed, elapsed_ms(started), false))
                        .await;
                }
                guard.finish(Phase::Failed, false);
                return Err(if error_value.is_timeout() {
                    error(
                        StatusCode::GATEWAY_TIMEOUT,
                        "llm-upstream-timeout",
                        "等待供应商响应超时。",
                    )
                } else {
                    error(
                        StatusCode::BAD_GATEWAY,
                        "llm-upstream-error",
                        "无法取得供应商响应。",
                    )
                });
            }
        };
        let status = upstream.status();
        let stream = upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        let identity = identity_encoding(upstream.headers());
        guard.response_headers(status.as_u16(), protocol, stream, identity);
        if let Some(writer) = &writer {
            writer
                .update(Update::Headers {
                    status: status.as_u16(),
                    stream,
                    identity,
                })
                .await;
        }
        let mut response_headers = forwarded_headers(upstream.headers());
        // 由 Axum 重新生成流式传输的消息边界。
        if !head && status != StatusCode::NOT_MODIFIED {
            response_headers.remove(header::CONTENT_LENGTH);
        }
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        *response.headers_mut() = response_headers;
        if head
            || matches!(
                status,
                StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
            )
        {
            let phase = if status.is_success() {
                Phase::Completed
            } else {
                Phase::Failed
            };
            if let Some(writer) = &writer {
                writer
                    .update(archive::finished(phase, elapsed_ms(started), false))
                    .await;
            }
            guard.finish(phase, false);
            return Ok(response);
        }

        let body = ForwardBody {
            upstream: Box::pin(upstream.bytes_stream()),
            guard: Some(guard),
            first_chunk: true,
            success: status.is_success(),
            writer,
            started,
        };
        *response.body_mut() = Body::from_stream(stream::unfold(body, |mut body| async move {
            body.guard.as_ref()?;
            match body.upstream.next().await {
                Some(Ok(chunk)) => {
                    body.observe(&chunk).await;
                    Some((Ok(chunk), body))
                }
                Some(Err(error_value)) => {
                    body.finish(false).await;
                    Some((Err(error_value), body))
                }
                None => {
                    body.finish(true).await;
                    None
                }
            }
        }));
        Ok(response)
    }
}

struct ForwardBody {
    upstream: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    guard: Option<RequestGuard>,
    first_chunk: bool,
    success: bool,
    writer: Option<Writer>,
    started: Instant,
}

impl ForwardBody {
    async fn observe(&mut self, chunk: &Bytes) {
        if !chunk.is_empty() && std::mem::take(&mut self.first_chunk) {
            self.guard.as_ref().expect("请求尚未结束").first_byte();
            if let Some(writer) = &self.writer {
                writer
                    .update(Update::FirstByte(elapsed_ms(self.started)))
                    .await;
            }
        }
        if let Some(guard) = &mut self.guard {
            guard.body_bytes(chunk);
        }
        if let Some(writer) = &self.writer {
            writer.response(chunk).await;
        }
    }

    async fn finish(&mut self, body_complete: bool) {
        let phase = if self.success && body_complete {
            Phase::Completed
        } else {
            Phase::Failed
        };
        if let Some(writer) = self.writer.take() {
            writer
                .update(archive::finished(
                    phase,
                    elapsed_ms(self.started),
                    body_complete,
                ))
                .await;
        }
        self.guard
            .take()
            .expect("请求尚未结束")
            .finish(phase, body_complete);
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn identity_encoding(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .all(|value| {
            value.to_str().is_ok_and(|codings| {
                codings
                    .split(',')
                    .all(|coding| coding.trim().eq_ignore_ascii_case("identity"))
            })
        })
}

fn forwarded_headers(headers: &HeaderMap) -> HeaderMap {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if !matches!(
            name.as_str(),
            "proxy-authorization"
                | "proxy-authenticate"
                | "connection"
                | "keep-alive"
                | "proxy-connection"
                | "transfer-encoding"
                | "te"
                | "trailer"
                | "upgrade"
        ) && !nominated.contains(name)
        {
            result.append(name.clone(), value.clone());
        }
    }
    result
}

async fn read_provider_token(
    config: &ConfigService,
    id: &str,
    record: &Value,
) -> Result<Option<String>, &'static str> {
    if !record[PROVIDER_TOKEN].is_string() {
        return Ok(None);
    }
    let [token] = config
        .read_record_passwords(MODULE, PROVIDERS, id, [PROVIDER_TOKEN])
        .await
        .map_err(|_| "llm-provider-key-missing")?;
    token.map(Some).ok_or("llm-provider-key-missing")
}

fn read_upstream(
    record: &Value,
    protocol: Protocol,
    token: &Result<Option<String>, &'static str>,
) -> Result<Option<Upstream>, &'static str> {
    let Some(uri) = provider_uri(record, protocol.uri_field()).map_err(|_| "llm-provider-uri")?
    else {
        return Ok(None);
    };
    let credential = if let Some(token) = token.as_ref().map_err(|&code| code)? {
        let value = match protocol {
            Protocol::Codex => HeaderValue::from_str(&format!("Bearer {token}")),
            Protocol::Claude => HeaderValue::from_str(token),
        };
        let mut header = value.map_err(|_| "llm-provider-key-invalid")?;
        header.set_sensitive(true);
        Some(header)
    } else {
        None
    };
    Ok(Some(Upstream { uri, credential }))
}

fn config_change_error(error_value: ConfigServiceError) -> Response {
    match error_value {
        ConfigServiceError::PendingValidation(detail) => error(
            StatusCode::BAD_REQUEST,
            "llm-invalid-client",
            detail.message,
        ),
        ConfigServiceError::RecordExists => error(
            StatusCode::CONFLICT,
            "llm-client-exists",
            "客户端名称已存在。",
        ),
        ConfigServiceError::InvalidValue(message) => {
            error(StatusCode::BAD_REQUEST, "llm-invalid-client", message)
        }
        _ => config_error(),
    }
}

fn config_error() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "llm-config-error",
        "无法读取或保存 LLM 配置。",
    )
}

fn invalid_token() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        "llm-invalid-token",
        "客户端令牌缺失或无效。",
    )
}
