pub mod secrets;

use axum::{
    extract::{ConnectInfo, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::net::{IpAddr, SocketAddr};

/// The development admin surface is local-only, including when the proxy binds 0.0.0.0.
pub async fn local_admin(req: Request, next: Next) -> Response {
    let peer_local = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|peer| peer.0.ip().is_loopback());
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let host_url = host.and_then(|h| reqwest::Url::parse(&format!("http://{h}")).ok());
    let host_local = host_url.as_ref().is_some_and(|url| {
        url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.host_str().is_some_and(|h| {
                h == "localhost"
                    || h.trim_matches(['[', ']'])
                        .parse::<IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
    });
    let origin_ok = match req.headers().get(header::ORIGIN) {
        None => true,
        Some(origin) => origin
            .to_str()
            .ok()
            .and_then(|s| reqwest::Url::parse(s).ok())
            .is_some_and(|url| {
                host_url
                    .as_ref()
                    .is_some_and(|local| url.origin() == local.origin())
            }),
    };
    let cross_site = req
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site");
    if !peer_local || !host_local || !origin_ok || cross_site {
        return (
            StatusCode::FORBIDDEN,
            Json(
                json!({"error": "管理台仅允许本机同源访问，请使用 http://127.0.0.1:端口/__admin"}),
            ),
        )
            .into_response();
    }
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    response
        .headers_mut()
        .insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
}
