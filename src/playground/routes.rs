use super::request::ChatRequest;
use crate::{security::secrets, state::AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

pub async fn page() -> Html<&'static str> {
    Html(include_str!("../../static/playground.html"))
}

fn local_error(status: StatusCode, kind: &str, message: &str) -> Response {
    (
        status,
        Json(
            json!({"ok": false, "http_status": null, "error": {"kind": kind, "message": message}}),
        ),
    )
        .into_response()
}

fn result_error(
    status: StatusCode,
    kind: &str,
    message: &str,
    started: Instant,
    upstream_status: Option<u16>,
    body: &ChatRequest,
) -> Response {
    (
        status,
        Json(json!({
            "ok": false, "provider_id": body.provider_id, "model": body.model,
            "http_status": upstream_status, "duration_ms": started.elapsed().as_millis() as u64,
            "error": {"kind": kind, "message": message},
        })),
    )
        .into_response()
}

pub async fn chat(State(state): State<AppState>, Json(body): Json<ChatRequest>) -> Response {
    let payload = match body.payload() {
        Ok(payload) => payload,
        Err(message) => return local_error(StatusCode::BAD_REQUEST, "validation", message),
    };
    let Some(provider) = state.providers.get(&body.provider_id).await else {
        return local_error(StatusCode::NOT_FOUND, "provider_not_found", "供应商不存在");
    };
    if !provider.models.contains(&body.model) {
        return local_error(
            StatusCode::BAD_REQUEST,
            "validation",
            "模型未配置在该供应商中",
        );
    }
    let secret = match secrets::resolve(provider.api_key_env.as_deref()) {
        Ok(secret) => secret,
        Err(message) => {
            return local_error(StatusCode::BAD_REQUEST, "missing_credentials", message)
        }
    };
    let mut request = state
        .http
        .post(provider.endpoint())
        .timeout(Duration::from_secs(provider.timeout_seconds))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(serde_json::to_vec(&payload).expect("serializable JSON"));
    if let Some(secret) = &secret {
        request = request.bearer_auth(secret);
    }
    let started = Instant::now();
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let (status, kind, message) = if error.is_timeout() {
                (StatusCode::GATEWAY_TIMEOUT, "timeout", "上游请求超时")
            } else {
                (
                    StatusCode::BAD_GATEWAY,
                    "connection",
                    "无法连接上游服务，请检查 Base URL、网络及 TLS 配置",
                )
            };
            return result_error(status, kind, message, started, None, &body);
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len() + chunk.len() > RESPONSE_LIMIT {
                    return result_error(
                        StatusCode::BAD_GATEWAY,
                        "response_too_large",
                        "上游响应超过 2 MiB 限制",
                        started,
                        Some(status.as_u16()),
                        &body,
                    );
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                let (code, kind, message) = if error.is_timeout() {
                    (StatusCode::GATEWAY_TIMEOUT, "timeout", "读取上游响应超时")
                } else {
                    (StatusCode::BAD_GATEWAY, "response_read", "上游响应传输中断")
                };
                return result_error(code, kind, message, started, Some(status.as_u16()), &body);
            }
        }
    }
    let parsed = serde_json::from_slice::<Value>(&bytes);
    let mut response_json = parsed.as_ref().ok().cloned();
    if let Some(value) = &mut response_json {
        secrets::redact_json(value, secret.as_deref());
    }
    let raw = parsed
        .is_err()
        .then(|| secrets::redact_text(&String::from_utf8_lossy(&bytes), secret.as_deref()));
    let valid_chat = response_json.as_ref().is_some_and(|v| {
        v.get("choices")
            .and_then(Value::as_array)
            .is_some_and(|choices| {
                !choices.is_empty()
                    && choices
                        .iter()
                        .all(|choice| choice.get("message").is_some_and(Value::is_object))
            })
            && v.get("error").is_none()
    });
    let ok = status.is_success() && valid_chat;
    let error_kind = if status.is_success() {
        "protocol"
    } else {
        match status.as_u16() {
            401 | 403 => "authentication",
            429 => "rate_limit",
            300..=399 => "redirect",
            500..=599 => "upstream_server",
            _ => "upstream_request",
        }
    };
    let error = (!ok).then(|| json!({
        "kind": error_kind,
        "message": if status.is_success() { "上游未返回有效的 Chat Completions JSON" } else { "上游调用失败，详见 response 或 raw_response" },
        "code": response_json.as_ref().and_then(|v| v.pointer("/error/code")).cloned(),
    }));
    let usage = response_json.as_ref().and_then(|v| v.get("usage")).cloned();
    let api_status = if status.is_success() && !ok || status.is_redirection() {
        StatusCode::BAD_GATEWAY
    } else {
        status
    };
    (
        api_status,
        Json(json!({
            "ok": ok, "provider_id": body.provider_id, "model": body.model,
            "http_status": status.as_u16(), "duration_ms": started.elapsed().as_millis() as u64,
            "response": response_json, "raw_response": raw, "usage": usage, "error": error,
        })),
    )
        .into_response()
}
