use std::{collections::BTreeMap, pin::Pin, sync::Arc, time::Duration};

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
    config::{CLIENTS, MODULE, PROVIDERS, provider_uri},
    instructions::Format,
    monitor::{Monitor, RequestGuard},
    protocol::{Binding, Client, IssuedClient, Phase, Protocol, error},
    usage::UsageReader,
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
    http: reqwest::Client,
}

impl LlmService {
    pub(crate) fn new(config: Arc<ConfigService>, monitor: Monitor) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            config,
            authorization: Process::start(Authorization::default()),
            monitor,
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
                        let mut resolved = [Ok(None), Ok(None)];
                        for (protocol, result) in [Protocol::Codex, Protocol::Claude]
                            .into_iter()
                            .zip(&mut resolved)
                        {
                            *result = read_upstream(&config, id, record, protocol).await;
                        }
                        let [codex, claude] = resolved;
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
                .map_err(|code| error(StatusCode::BAD_GATEWAY, code, "供应商的 API Key 不可用。"))?
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
        let observe_request = track_usage && identity_encoding(&parts.headers);
        let instruction_format = track_usage
            .then(|| Format::for_endpoint(protocol, endpoint))
            .flatten();
        let mut guard = self.monitor.start(
            client,
            protocol,
            &parts.headers,
            track_usage,
            instruction_format,
        );
        let mut upstream = self.http.request(parts.method, uri).headers(headers);
        if !body.is_end_stream() {
            let length = body.size_hint().exact();
            let body = body.into_data_stream();
            let body = if observe_request {
                let observed = stream::unfold(
                    (body, guard.request_body_observer(), length),
                    |(mut body, mut observer, mut remaining)| async move {
                        match body.next().await {
                            Some(item) => {
                                if let Ok(chunk) = &item {
                                    observer.push(chunk);
                                    remaining = remaining
                                        .and_then(|size| size.checked_sub(chunk.len() as u64));
                                    if remaining == Some(0) {
                                        observer.complete();
                                    }
                                } else {
                                    observer.abort();
                                }
                                Some((item, (body, observer, remaining)))
                            }
                            None => {
                                if matches!(remaining, None | Some(0)) {
                                    observer.complete();
                                }
                                None
                            }
                        }
                    },
                );
                reqwest::Body::wrap_stream(observed)
            } else {
                reqwest::Body::wrap_stream(body)
            };
            upstream = upstream.body(body);
        }
        let upstream = match upstream.send().await {
            Ok(response) => response,
            Err(error_value) => {
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
            guard.finish(phase, false);
            return Ok(response);
        }

        let body = ForwardBody {
            upstream: Box::pin(upstream.bytes_stream()),
            guard: Some(guard),
            first_chunk: true,
            success: status.is_success(),
        };
        *response.body_mut() = Body::from_stream(stream::unfold(body, |mut body| async move {
            body.guard.as_ref()?;
            match body.upstream.next().await {
                Some(Ok(chunk)) => {
                    body.observe(&chunk);
                    Some((Ok(chunk), body))
                }
                Some(Err(error_value)) => {
                    body.finish(false);
                    Some((Err(error_value), body))
                }
                None => {
                    body.finish(true);
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
}

impl ForwardBody {
    fn observe(&mut self, chunk: &Bytes) {
        if !chunk.is_empty() && std::mem::take(&mut self.first_chunk) {
            self.guard.as_ref().expect("请求尚未结束").first_byte();
        }
        if let Some(guard) = &mut self.guard {
            guard.body_bytes(chunk);
        }
    }

    fn finish(&mut self, body_complete: bool) {
        let phase = if self.success && body_complete {
            Phase::Completed
        } else {
            Phase::Failed
        };
        self.guard
            .take()
            .expect("请求尚未结束")
            .finish(phase, body_complete);
    }
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

async fn read_upstream(
    config: &ConfigService,
    id: &str,
    record: &Value,
    protocol: Protocol,
) -> Result<Option<Upstream>, &'static str> {
    let Some(uri) = provider_uri(record, protocol.uri_field()).map_err(|_| "llm-provider-uri")?
    else {
        return Ok(None);
    };
    let credential = if record[protocol.key_field()].is_string() {
        let [key] = config
            .read_record_passwords(MODULE, PROVIDERS, id, [protocol.key_field()])
            .await
            .map_err(|_| "llm-provider-key-missing")?;
        let key = key.ok_or("llm-provider-key-missing")?;
        let value = match protocol {
            Protocol::Codex => format!("Bearer {key}"),
            Protocol::Claude => key,
        };
        let mut header = HeaderValue::from_str(&value).map_err(|_| "llm-provider-key-invalid")?;
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
