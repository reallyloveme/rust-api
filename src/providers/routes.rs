use super::{config::Provider, registry::RegistryError};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

fn registry_error(error: RegistryError) -> Response {
    let (status, message) = match error {
        RegistryError::Invalid(message) => (StatusCode::BAD_REQUEST, message),
        RegistryError::NotFound => (StatusCode::NOT_FOUND, "供应商不存在".into()),
        RegistryError::Conflict => (StatusCode::CONFLICT, "供应商 id 已存在".into()),
        RegistryError::Storage => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "供应商配置保存失败；未应用本次修改".into(),
        ),
    };
    (status, Json(json!({"error": message}))).into_response()
}

pub async fn list(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({"providers": state.providers.list().await}))
}

pub async fn get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.providers.get(&id).await {
        Some(provider) => Json(provider).into_response(),
        None => registry_error(RegistryError::NotFound),
    }
}

pub async fn create(State(state): State<AppState>, Json(provider): Json<Provider>) -> Response {
    match state.providers.save(provider, true).await {
        Ok(provider) => (StatusCode::CREATED, Json(provider)).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(provider): Json<Provider>,
) -> Response {
    if id != provider.id {
        return registry_error(RegistryError::Invalid("路径 id 与请求 id 不一致".into()));
    }
    match state.providers.save(provider, false).await {
        Ok(provider) => Json(provider).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.providers.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => registry_error(error),
    }
}
