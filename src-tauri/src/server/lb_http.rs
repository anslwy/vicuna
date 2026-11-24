use futures_util::TryStreamExt;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::{Method, Response, body::Frame, header};
use std::{sync::OnceLock, time::Duration};

pub(crate) type BoxBody = http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, reqwest::Error>;

pub(crate) fn full_body(data: impl Into<bytes::Bytes>) -> BoxBody {
    Full::new(data.into())
        .map_err(|never| match never {})
        .boxed_unsync()
}

pub(crate) fn is_viewport_request(method: &Method, lb_path: &str) -> bool {
    *method == Method::GET && lb_path == "/api/viewport-stream"
}

pub(crate) fn client(viewport: bool) -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    static VIEWPORT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = if viewport { &VIEWPORT_CLIENT } else { &CLIENT };
    client.get_or_init(|| {
        let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        let builder = if viewport {
            // Live Browser sends a heartbeat every 5 seconds when idle.
            // Time out stalled reads without imposing a lifetime on the stream.
            builder
                .connect_timeout(Duration::from_secs(5))
                .read_timeout(Duration::from_secs(20))
                .no_gzip()
        } else {
            builder
        };
        builder.build().expect("reqwest client")
    })
}

pub(crate) async fn response(
    upstream: reqwest::Response,
    viewport: bool,
) -> Option<Response<BoxBody>> {
    let status = upstream.status();
    let mut builder = Response::builder().status(status);
    for (name, value) in upstream.headers() {
        if name == header::TRANSFER_ENCODING
            || name == header::CONNECTION
            || name == header::CONTENT_LENGTH
        {
            continue;
        }
        builder = builder.header(name, value);
    }

    let body = if viewport && status.is_success() {
        // Hyper polls the upstream directly. Dropping the downstream body
        // drops the upstream reader, and read errors terminate the response.
        StreamBody::new(upstream.bytes_stream().map_ok(Frame::data)).boxed_unsync()
    } else {
        full_body(upstream.bytes().await.unwrap_or_default())
    };
    builder.body(body).ok()
}
