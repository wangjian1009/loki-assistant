use axum::{
    body::{Body, Bytes},
    http::header::CONTENT_TYPE,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use futures_util::{Stream, StreamExt};
use schemars::{JsonSchema, schema_for};
use serde::Serialize;

pub(crate) const MEDIA_TYPE: &str = "text/event-stream";
pub(crate) const EVENT_LIMIT: usize = 1_048_576;

/// SSE 事件的名称和 JSON 数据类型。
pub trait SseEvent: Serialize + JsonSchema {
    const EVENT: &'static str;
}

pub fn metadata<T: SseEvent>() -> crate::modules::discover::model::SseEventMetadata {
    crate::modules::discover::model::SseEventMetadata {
        event: T::EVENT,
        data_schema: serde_json::to_value(schema_for!(T)).expect("JSON Schema 可以序列化"),
    }
}

/// 仅由已认证的 iroh 请求分发器设置，请求 handler 使用 JSON 事件帧。
#[derive(Clone, Copy)]
pub(crate) struct RemoteSse;

/// 标记已经按 iroh 帧协议编码的响应。
#[derive(Clone, Copy)]
pub(crate) struct FramedSse;

pub(crate) fn response<T: SseEvent>(
    events: impl Stream<Item = T> + Send + 'static,
    remote: Option<RemoteSse>,
) -> Response {
    if remote.is_some() {
        #[derive(Serialize)]
        struct Frame<T> {
            event: &'static str,
            data: T,
        }

        let frames = events.map(|data| {
            encode_frame(
                &Frame {
                    event: T::EVENT,
                    data,
                },
                EVENT_LIMIT,
            )
            .ok_or_else(|| std::io::Error::other("SSE 事件无法序列化或超过大小限制"))
        });
        (
            [(CONTENT_TYPE, MEDIA_TYPE)],
            axum::Extension(FramedSse),
            Body::from_stream(frames),
        )
            .into_response()
    } else {
        Sse::new(events.map(|data| Event::default().event(T::EVENT).json_data(data)))
            .keep_alive(KeepAlive::default())
            .into_response()
    }
}

pub(crate) fn encode_frame(value: &impl Serialize, max_len: usize) -> Option<Bytes> {
    let mut frame = vec![0; 4];
    serde_json::to_writer(&mut frame, value).ok()?;
    let len = frame.len() - 4;
    if len > max_len {
        return None;
    }
    frame[..4].copy_from_slice(&(len as u32).to_be_bytes());
    Some(frame.into())
}
