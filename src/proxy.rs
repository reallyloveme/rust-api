use crate::state::{extract_origin, AppState, RequestLog, ResolveError};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use serde_json::json;
use std::time::Instant;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

const PREVIEW_MAX: usize = 8000;

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| h.eq_ignore_ascii_case(name))
}

pub fn build_upstream_url(
    target: &str,
    path: &str,
    query: Option<&str>,
    strip_prefix: bool,
    prefix: &str,
) -> String {
    let forward_path = if strip_prefix {
        let stripped = path.strip_prefix(prefix).unwrap_or(path);
        if stripped.is_empty() {
            "/"
        } else if stripped.starts_with('/') {
            stripped
        } else {
            path
        }
    } else {
        path
    };

    match query {
        Some(q) if !q.is_empty() => format!("{target}{forward_path}?{q}"),
        _ => format!("{target}{forward_path}"),
    }
}

/// 去掉 Domain；若前端是 http，再去掉 Secure，方便本地落 Cookie
pub fn rewrite_set_cookie(value: &str, origin_is_http: bool) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .enumerate()
        .filter_map(|(i, p)| {
            let lower = p.to_ascii_lowercase();
            if i > 0 && lower.starts_with("domain=") {
                return None;
            }
            if origin_is_http && lower == "secure" {
                return None;
            }
            Some(p)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn body_preview(content_type: Option<&str>, body: &[u8]) -> Option<String> {
    let ct = content_type.unwrap_or("").to_ascii_lowercase();
    let textish = ct.contains("json")
        || ct.contains("text/")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("x-www-form-urlencoded")
        || ct.is_empty();
    if !textish || body.is_empty() || body.len() > 256 * 1024 {
        return None;
    }
    let s = String::from_utf8_lossy(body);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let preview: String = trimmed.chars().take(PREVIEW_MAX).collect();
    if trimmed.chars().count() > PREVIEW_MAX {
        Some(format!("{preview}…"))
    } else {
        Some(preview)
    }
}

pub fn request_preview(query: Option<&str>, content_type: Option<&str>, body: &[u8]) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(q) = query.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("?{q}"));
    }
    if let Some(b) = body_preview(content_type, body) {
        parts.push(b);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

pub async fn proxy_handler(State(state): State<AppState>, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    let query = uri.query().map(|s| s.to_string());
    let headers = req.headers().clone();
    let origin = extract_origin(&headers);
    let origin_str = origin.clone().unwrap_or_default();
    let origin_is_http = origin_str.starts_with("http://");

    let resolved = state.resolve(origin.as_deref(), &path).await;
    let (frontend_id, env_name, rule) = match resolved {
        Ok(v) => v,
        Err(e) => {
            let (code, body) = match e {
                ResolveError::MissingOrigin => (
                    StatusCode::BAD_REQUEST,
                    json!({
                        "error": "missing origin",
                        "hint": "浏览器请求需带 Origin；推荐前端直连本代理；或仅配置一个前端后可用 curl",
                        "path": path,
                    }),
                ),
                ResolveError::UnknownOrigin(o) => (
                    StatusCode::FORBIDDEN,
                    json!({
                        "error": "unknown frontend origin",
                        "origin": o,
                        "path": path,
                        "hint": "在管理台为该 Origin 新建前端并配置规则（注意 localhost 与 127.0.0.1 不同）",
                    }),
                ),
                ResolveError::NoEnv(f, env) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "active env missing", "frontend": f, "env": env }),
                ),
                ResolveError::NoRule(f, env, p) => (
                    StatusCode::NOT_FOUND,
                    json!({
                        "error": "no matching proxy rule",
                        "frontend": f,
                        "env": env,
                        "path": p,
                    }),
                ),
            };
            return (code, Json(body)).into_response();
        }
    };

    let rewrite_cookies = {
        let frontends = state.frontends.read().await;
        frontends
            .get(&frontend_id)
            .map(|f| f.rewrite_cookies)
            .unwrap_or(false)
    };

    let upstream_url = build_upstream_url(
        &rule.target,
        &path,
        query.as_deref(),
        rule.strip_prefix,
        &rule.match_prefix,
    );

    let body_bytes = match axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("读取请求体失败: {e}") })),
            )
                .into_response();
        }
    };

    let req_ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    let req_preview = request_preview(query.as_deref(), req_ct, &body_bytes);

    let mut upstream_req = state
        .http
        .request(
            reqwest::Method::from_bytes(method.as_str().as_bytes()).unwrap_or(reqwest::Method::GET),
            &upstream_url,
        )
        .body(body_bytes.to_vec());

    let mut req_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        req_headers.insert(name.clone(), value.clone());
    }
    for (k, v) in &rule.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(k.as_str()),
            HeaderValue::from_str(v),
        ) {
            req_headers.insert(name, value);
        }
    }
    upstream_req = upstream_req.headers(reqwest_headers_from_axum(&req_headers));

    let started = Instant::now();
    let result = upstream_req.send().await;
    let duration_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(upstream) => {
            let status = upstream.status().as_u16();
            let upstream_headers = upstream.headers().clone();
            let content_type = upstream_headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let body = match upstream.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    state
                        .push_log(RequestLog {
                            time: Utc::now(),
                            method: method.to_string(),
                            path: path.clone(),
                            target: upstream_url.clone(),
                            status: 502,
                            duration_ms,
                            rule: rule.name.clone(),
                            env: env_name.clone(),
                            frontend: frontend_id.clone(),
                            origin: origin_str.clone(),
                            req_preview: req_preview.clone(),
                            body_preview: None,
                        })
                        .await;
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({ "error": format!("读取上游响应失败: {e}") })),
                    )
                        .into_response();
                }
            };

            let preview = body_preview(content_type.as_deref(), &body);

            state
                .push_log(RequestLog {
                    time: Utc::now(),
                    method: method.to_string(),
                    path: path.clone(),
                    target: upstream_url,
                    status,
                    duration_ms,
                    rule: rule.name.clone(),
                    env: env_name.clone(),
                    frontend: frontend_id.clone(),
                    origin: origin_str.clone(),
                    req_preview: req_preview.clone(),
                    body_preview: preview,
                })
                .await;

            let mut response = Response::builder().status(status);
            let resp_headers = response.headers_mut().unwrap();
            for (name, value) in upstream_headers.iter() {
                if is_hop_by_hop(name.as_str()) {
                    continue;
                }
                if name.as_str().eq_ignore_ascii_case("set-cookie") {
                    continue;
                }
                if let (Ok(n), Ok(v)) = (
                    header::HeaderName::try_from(name.as_str()),
                    HeaderValue::from_bytes(value.as_bytes()),
                ) {
                    resp_headers.insert(n, v);
                }
            }
            // Set-Cookie 可能多条，单独处理
            for value in upstream_headers.get_all(header::SET_COOKIE) {
                let raw = match value.to_str() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let rewritten = if rewrite_cookies {
                    rewrite_set_cookie(raw, origin_is_http)
                } else {
                    raw.to_string()
                };
                if let Ok(v) = HeaderValue::from_str(&rewritten) {
                    resp_headers.append(header::SET_COOKIE, v);
                }
            }
            response.body(Body::from(body)).unwrap_or_else(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "构建响应失败" })),
                )
                    .into_response()
            })
        }
        Err(e) => {
            state
                .push_log(RequestLog {
                    time: Utc::now(),
                    method: method.to_string(),
                    path,
                    target: upstream_url,
                    status: 502,
                    duration_ms,
                    rule: rule.name,
                    env: env_name,
                    frontend: frontend_id,
                    origin: origin_str,
                    req_preview,
                    body_preview: Some(format!("上游错误: {e}")),
                })
                .await;
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("上游请求失败: {e}") })),
            )
                .into_response()
        }
    }
}

fn reqwest_headers_from_axum(headers: &HeaderMap) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter() {
        if let (Ok(n), Ok(v)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
        ) {
            out.insert(n, v);
        }
    }
    out
}
