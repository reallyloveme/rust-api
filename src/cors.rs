use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};

/// 按 AppState.cors_origins 动态响应 CORS（管理台改 Origins 后立即生效）
pub async fn dynamic_cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let req_origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let acrh = req
        .headers()
        .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
        .cloned();
    let is_options = req.method() == Method::OPTIONS;

    let origins = state.cors_origins.read().await;
    let allow_any = origins.iter().any(|o| o.trim() == "*");
    let origin_allowed = match &req_origin {
        None => true,
        Some(o) => allow_any || origins.iter().any(|a| a.trim() == o.as_str()),
    };
    drop(origins);

    if is_options {
        let mut res = Response::builder().status(StatusCode::NO_CONTENT);
        if origin_allowed {
            inject_cors(res.headers_mut().unwrap(), &req_origin, allow_any, acrh.as_ref());
        }
        return res
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }

    let mut res = next.run(req).await;
    if origin_allowed && req_origin.is_some() {
        inject_cors(res.headers_mut(), &req_origin, allow_any, None);
    }
    res
}

fn inject_cors(
    headers: &mut axum::http::HeaderMap,
    req_origin: &Option<String>,
    allow_any: bool,
    request_headers: Option<&HeaderValue>,
) {
    if allow_any {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
    } else if let Some(o) = req_origin {
        if let Ok(v) = HeaderValue::from_str(o) {
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
                HeaderValue::from_static("true"),
            );
            headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        }
    }

    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET,POST,PUT,PATCH,DELETE,OPTIONS,HEAD"),
    );

    if let Some(h) = request_headers {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, h.clone());
    } else {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "authorization,content-type,accept,x-requested-with,cookie,origin",
            ),
        );
    }

    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("content-disposition"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
}
