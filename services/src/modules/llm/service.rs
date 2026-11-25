use std::{collections::BTreeMap, pin::Pin, sync::Arc, time::Duration};

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use super::{
    config::{MODULE, PROVIDERS, provider_uri},
    monitor::{Monitor, RequestGuard},
    protocol::{Binding, Client, IssuedClient, Phase, Protocol, error},
    usage::UsageReader,
};
use crate::modules::config::service::{ConfigService, ConfigServiceError};

const CLIENTS: &str = "llm_clients";
const TOKEN_PREFIX: &str = "loki-llm-";
const API_KEY: &str = "x-api-key";

#[derive(Deserialize, Serialize)]
struct StoredClient {
    digest: [u8; 32],
    binding: Binding,
}

type Clients = BTreeMap<Uuid, StoredClient>;

#[derive(Debug)]
pub(crate) struct LlmService {
    config: Arc<ConfigService>,
    pub(super) monitor: Monitor,
    http: reqwest::Client,
}

impl LlmService {
    pub(crate) fn new(config: Arc<ConfigService>, monitor: Monitor) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Self {
            config,
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

    pub(super) async fn clients(&self) -> Result<Vec<Client>, Response> {
        Ok(self
            .load_clients()
            .await?
            .into_iter()
            .map(|(id, stored)| Client {
                id,
                binding: stored.binding,
            })
            .collect())
    }

    pub(super) async fn issue(&self, binding: Binding) -> Result<IssuedClient, Response> {
        let binding = self.validate_binding(binding).await?;
        let id = Uuid::new_v4();
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "llm-random-error",
                "无法生成客户端令牌。",
            )
        })?;
        let token = format!("{TOKEN_PREFIX}{id}.{}", URL_SAFE_NO_PAD.encode(secret));
        let digest = Sha256::digest(token.as_bytes()).into();
        let stored = StoredClient {
            digest,
            binding: binding.clone(),
        };
        self.update_clients(move |clients| {
            clients.insert(id, stored);
            Some(())
        })
        .await?;
        Ok(IssuedClient {
            client: Client { id, binding },
            token,
        })
    }

    pub(super) async fn switch(&self, id: Uuid, binding: Binding) -> Result<Client, Response> {
        let binding = self.validate_binding(binding).await?;
        let next = binding.clone();
        self.update_clients(move |clients| {
            clients.get_mut(&id)?.binding = next;
            Some(())
        })
        .await?;
        Ok(Client { id, binding })
    }

    pub(super) async fn revoke(&self, id: Uuid) -> Result<(), Response> {
        self.update_clients(move |clients| clients.remove(&id).map(|_| ()))
            .await
    }

    async fn load_clients(&self) -> Result<Clients, Response> {
        decode_clients(self.config.system_value(CLIENTS).await).map_err(|_| config_error())
    }

    async fn update_clients(
        &self,
        update: impl FnOnce(&mut Clients) -> Option<()> + Send + 'static,
    ) -> Result<(), Response> {
        let updated = self
            .config
            .update_system_value(CLIENTS, None, async move |previous| {
                let mut clients = decode_clients(previous.clone())?;
                if update(&mut clients).is_none() {
                    return Ok((previous, false));
                }
                let value = serde_json::to_value(clients).map_err(client_storage_error)?;
                Ok((Some(value), true))
            })
            .await
            .map_err(|_| config_error())?;
        if !updated {
            return Err(error(
                StatusCode::NOT_FOUND,
                "llm-client-not-found",
                "客户端不存在。",
            ));
        }
        Ok(())
    }

    async fn validate_binding(&self, binding: Binding) -> Result<Binding, Response> {
        let providers = self
            .config
            .value(MODULE, PROVIDERS)
            .await
            .map_err(|_| config_error())?;
        if !providers.value.as_array().is_some_and(|providers| {
            providers
                .iter()
                .any(|record| record["id"].as_str() == Some(&binding.upstream))
        }) {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "llm-provider-not-found",
                "供应商不存在。",
            ));
        }
        Ok(binding)
    }

    async fn authenticate(
        &self,
        headers: &HeaderMap,
        protocol: Protocol,
    ) -> Result<Client, Response> {
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
            .ok_or_else(invalid_token)?;
        let (id, _) = token
            .strip_prefix(TOKEN_PREFIX)
            .and_then(|value| value.split_once('.'))
            .filter(|(_, secret)| secret.len() == 43)
            .ok_or_else(invalid_token)?;
        let id = Uuid::parse_str(id).map_err(|_| invalid_token())?;
        let mut clients = self.load_clients().await?;
        let stored = clients.remove(&id).ok_or_else(invalid_token)?;
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        if !bool::from(digest.ct_eq(&stored.digest)) {
            return Err(invalid_token());
        }
        Ok(Client {
            id,
            binding: stored.binding,
        })
    }

    pub(super) async fn forward(
        &self,
        protocol: Protocol,
        request: Request,
    ) -> Result<Response, Response> {
        let client = self.authenticate(request.headers(), protocol).await?;
        let (provider, [api_key]) = self
            .config
            .read_record_with_passwords(
                MODULE,
                PROVIDERS,
                &client.binding.upstream,
                [protocol.key_field()],
            )
            .await
            .map_err(|error_value| match error_value {
                ConfigServiceError::ActionNotFound => error(
                    StatusCode::BAD_GATEWAY,
                    "llm-provider-not-found",
                    "当前绑定的供应商不存在。",
                ),
                _ => config_error(),
            })?;
        let mut uri = provider_uri(&provider, protocol.uri_field())
            .map_err(|message| error(StatusCode::BAD_GATEWAY, "llm-provider-uri", message))?
            .ok_or_else(|| {
                error(
                    StatusCode::BAD_GATEWAY,
                    "llm-protocol-unavailable",
                    "当前供应商未配置此协议。",
                )
            })?;
        if provider[protocol.key_field()].is_string() && api_key.is_none() {
            return Err(error(
                StatusCode::BAD_GATEWAY,
                "llm-provider-key-missing",
                "供应商的 API Key 条目不可用。",
            ));
        }
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
        if let Some(key) = api_key {
            let (name, value) = match protocol {
                Protocol::Codex => (header::AUTHORIZATION, format!("Bearer {key}")),
                Protocol::Claude => (HeaderName::from_static(API_KEY), key),
            };
            let mut value = HeaderValue::from_str(&value).map_err(|_| {
                error(
                    StatusCode::BAD_GATEWAY,
                    "llm-provider-key-invalid",
                    "供应商的 API Key 格式无效。",
                )
            })?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }

        let head = parts.method == Method::HEAD;
        let track_usage = UsageReader::tracks(&parts.method, endpoint);
        let mut guard = self
            .monitor
            .start(client, protocol, endpoint.to_owned(), track_usage);
        let mut upstream = self.http.request(parts.method, uri).headers(headers);
        if !body.is_end_stream() {
            upstream = upstream.body(reqwest::Body::wrap_stream(body.into_data_stream()));
        }
        let upstream = match upstream.send().await {
            Ok(response) => response,
            Err(error_value) => {
                guard.finish(Phase::Failed, None);
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
        guard.upstream_status(status.as_u16());
        let stream = upstream
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/event-stream"));
        let identity = upstream
            .headers()
            .get_all(header::CONTENT_ENCODING)
            .iter()
            .all(|value| {
                value.to_str().is_ok_and(|codings| {
                    codings
                        .split(',')
                        .all(|coding| coding.trim().eq_ignore_ascii_case("identity"))
                })
            });
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
            guard.finish(phase, None);
            return Ok(response);
        }

        let body = ForwardBody {
            upstream: Box::pin(upstream.bytes_stream()),
            guard: Some(guard),
            usage: (track_usage && identity).then(|| UsageReader::new(protocol, stream)),
            stream,
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
    usage: Option<UsageReader>,
    stream: bool,
    first_chunk: bool,
    success: bool,
}

impl ForwardBody {
    fn observe(&mut self, chunk: &Bytes) {
        if !chunk.is_empty() && std::mem::take(&mut self.first_chunk) {
            self.guard
                .as_ref()
                .expect("请求尚未结束")
                .first_byte(self.stream);
        }
        if let Some(usage) = &mut self.usage {
            usage.push(chunk);
        }
    }

    fn finish(&mut self, body_complete: bool) {
        let observed = self.usage.take().map(|usage| usage.finish(body_complete));
        let succeeded =
            self.success && body_complete && !observed.is_some_and(|(_, failed)| failed);
        let phase = if succeeded {
            Phase::Completed
        } else {
            Phase::Failed
        };
        self.guard
            .take()
            .expect("请求尚未结束")
            .finish(phase, observed.map(|(usage, _)| usage));
    }
}

impl Drop for ForwardBody {
    fn drop(&mut self) {
        if let Some(guard) = &mut self.guard {
            guard.finish(
                Phase::Cancelled,
                self.usage.take().map(|usage| usage.finish(false).0),
            );
        }
    }
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

fn decode_clients(value: Option<Value>) -> Result<Clients, ConfigServiceError> {
    value
        .map(serde_json::from_value)
        .transpose()
        .map(Option::unwrap_or_default)
        .map_err(client_storage_error)
}

fn client_storage_error(_: serde_json::Error) -> ConfigServiceError {
    ConfigServiceError::Storage("LLM 客户端配置格式无效。".into())
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
