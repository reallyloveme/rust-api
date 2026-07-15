use crate::config::{Config, EnvConfig, FrontendConfig, Rule};
use crate::state::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use serde::Deserialize;
use serde_json::json;

const ADMIN_HTML: &str = include_str!("../static/admin.html");

pub async fn admin_page() -> Html<&'static str> {
    Html(ADMIN_HTML)
}

fn bad(msg: impl Into<String>) -> axum::response::Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.into() }))).into_response()
}

fn save_apply(state: &AppState, config: &Config) -> Result<(), String> {
    config.save(state.config_path.as_str())
}

#[derive(Deserialize)]
pub struct FrontendQuery {
    pub frontend: Option<String>,
}

pub async fn list_frontends(State(state): State<AppState>) -> impl IntoResponse {
    let frontends = state.frontends.read().await;
    let mut list: Vec<_> = frontends
        .iter()
        .map(|(id, fe)| {
            json!({
                "id": id,
                "label": if fe.label.is_empty() { id.clone() } else { fe.label.clone() },
                "origins": fe.origins,
                "active_env": fe.active_env,
                "rewrite_cookies": fe.rewrite_cookies,
                "env_count": fe.envs.len(),
                "rule_count": fe.envs.get(&fe.active_env).map(|e| e.rules.len()).unwrap_or(0),
            })
        })
        .collect();
    list.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or("")
            .cmp(b["id"].as_str().unwrap_or(""))
    });
    Json(json!({ "frontends": list }))
}

#[derive(Deserialize)]
pub struct CreateFrontendBody {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default)]
    pub copy_from: Option<String>,
}

pub async fn create_frontend(
    State(state): State<AppState>,
    Json(body): Json<CreateFrontendBody>,
) -> impl IntoResponse {
    let id = body.id.trim().to_string();
    if id.is_empty() || id.contains('/') {
        return bad("前端 id 无效");
    }
    let mut config = state.snapshot_config().await;
    if config.frontends.contains_key(&id) {
        return bad(format!("前端 '{id}' 已存在"));
    }

    let (envs, active_env, rewrite_cookies) = if let Some(from) = body
        .copy_from
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        match config.frontends.get(from) {
            Some(fe) => (fe.envs.clone(), fe.active_env.clone(), fe.rewrite_cookies),
            None => return bad(format!("复制源前端 '{from}' 不存在")),
        }
    } else {
        let mut envs = std::collections::HashMap::new();
        envs.insert(
            "default".into(),
            EnvConfig {
                label: "默认".into(),
                rules: vec![],
            },
        );
        (envs, "default".into(), false)
    };

    let label = if body.label.trim().is_empty() {
        id.clone()
    } else {
        body.label.trim().to_string()
    };
    let origins: Vec<String> = body
        .origins
        .into_iter()
        .map(|o| o.trim().trim_end_matches('/').to_string())
        .filter(|o| !o.is_empty())
        .collect();

    config.frontends.insert(
        id.clone(),
        FrontendConfig {
            label,
            origins,
            active_env,
            envs,
            rewrite_cookies,
        },
    );
    if let Err(e) = config.validate() {
        return bad(e);
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    Json(json!({ "ok": true, "id": id })).into_response()
}

#[derive(Deserialize)]
pub struct DeleteFrontendBody {
    pub id: String,
}

pub async fn delete_frontend(
    State(state): State<AppState>,
    Json(body): Json<DeleteFrontendBody>,
) -> impl IntoResponse {
    let id = body.id.trim().to_string();
    let mut config = state.snapshot_config().await;
    if config.frontends.len() <= 1 {
        return bad("至少保留一个前端");
    }
    if config.frontends.remove(&id).is_none() {
        return bad(format!("前端 '{id}' 不存在"));
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
pub struct UpdateFrontendBody {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub origins: Option<Vec<String>>,
    #[serde(default)]
    pub rewrite_cookies: Option<bool>,
}

pub async fn update_frontend(
    State(state): State<AppState>,
    Json(body): Json<UpdateFrontendBody>,
) -> impl IntoResponse {
    let id = body.id.trim().to_string();
    let mut config = state.snapshot_config().await;
    let Some(fe) = config.frontends.get_mut(&id) else {
        return bad(format!("前端 '{id}' 不存在"));
    };
    if let Some(label) = body.label {
        if !label.trim().is_empty() {
            fe.label = label.trim().to_string();
        }
    }
    if let Some(origins) = body.origins {
        fe.origins = origins
            .into_iter()
            .map(|o| o.trim().trim_end_matches('/').to_string())
            .filter(|o| !o.is_empty())
            .collect();
    }
    if let Some(v) = body.rewrite_cookies {
        fe.rewrite_cookies = v;
    }
    if let Err(e) = config.validate() {
        return bad(e);
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    Json(json!({ "ok": true })).into_response()
}

pub async fn list_envs(
    State(state): State<AppState>,
    Query(q): Query<FrontendQuery>,
) -> impl IntoResponse {
    let Some(fid) = q.frontend.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return bad("缺少 frontend 参数");
    };
    let frontends = state.frontends.read().await;
    let Some(fe) = frontends.get(fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    let mut list: Vec<_> = fe
        .envs
        .iter()
        .map(|(id, env)| {
            json!({
                "id": id,
                "label": if env.label.is_empty() { id.clone() } else { env.label.clone() },
                "rule_count": env.rules.len(),
            })
        })
        .collect();
    list.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or("")
            .cmp(b["id"].as_str().unwrap_or(""))
    });
    Json(json!({
        "frontend": fid,
        "active": fe.active_env,
        "envs": list,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct ActiveBody {
    pub frontend: String,
    pub id: String,
}

pub async fn set_active(
    State(state): State<AppState>,
    Json(body): Json<ActiveBody>,
) -> impl IntoResponse {
    let fid = body.frontend.trim().to_string();
    let eid = body.id.trim().to_string();
    let mut config = state.snapshot_config().await;
    let Some(fe) = config.frontends.get_mut(&fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    if !fe.envs.contains_key(&eid) {
        return bad(format!("环境 '{eid}' 不存在"));
    }
    fe.active_env = eid.clone();
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    tracing::info!("前端 {fid} 切换环境 -> {eid}");
    Json(json!({ "ok": true, "frontend": fid, "active": eid })).into_response()
}

#[derive(Deserialize)]
pub struct CreateEnvBody {
    pub frontend: String,
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub copy_from: Option<String>,
}

pub async fn create_env(
    State(state): State<AppState>,
    Json(body): Json<CreateEnvBody>,
) -> impl IntoResponse {
    let fid = body.frontend.trim().to_string();
    let eid = body.id.trim().to_string();
    if eid.is_empty() {
        return bad("环境 id 不能为空");
    }
    let mut config = state.snapshot_config().await;
    let Some(fe) = config.frontends.get_mut(&fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    if fe.envs.contains_key(&eid) {
        return bad(format!("环境 '{eid}' 已存在"));
    }
    let rules = if let Some(from) = body.copy_from.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty())
    {
        match fe.envs.get(from) {
            Some(e) => e.rules.clone(),
            None => return bad(format!("复制源环境 '{from}' 不存在")),
        }
    } else {
        vec![]
    };
    let label = if body.label.trim().is_empty() {
        eid.clone()
    } else {
        body.label.trim().to_string()
    };
    fe.envs.insert(eid.clone(), EnvConfig { label, rules });
    if let Err(e) = config.validate() {
        return bad(e);
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    Json(json!({ "ok": true, "id": eid })).into_response()
}

#[derive(Deserialize)]
pub struct DeleteEnvBody {
    pub frontend: String,
    pub id: String,
}

pub async fn delete_env(
    State(state): State<AppState>,
    Json(body): Json<DeleteEnvBody>,
) -> impl IntoResponse {
    let fid = body.frontend.trim().to_string();
    let eid = body.id.trim().to_string();
    let mut config = state.snapshot_config().await;
    let Some(fe) = config.frontends.get_mut(&fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    if fe.envs.len() <= 1 {
        return bad("至少保留一个环境");
    }
    if fe.active_env == eid {
        return bad("不能删除当前生效环境，请先切换");
    }
    if fe.envs.remove(&eid).is_none() {
        return bad(format!("环境 '{eid}' 不存在"));
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    Json(json!({ "ok": true })).into_response()
}

pub async fn get_rules(
    State(state): State<AppState>,
    Query(q): Query<FrontendQuery>,
) -> impl IntoResponse {
    let Some(fid) = q.frontend.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return bad("缺少 frontend 参数");
    };
    let frontends = state.frontends.read().await;
    let Some(fe) = frontends.get(fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    let rules = fe
        .envs
        .get(&fe.active_env)
        .map(|e| e.rules.clone())
        .unwrap_or_default();
    Json(json!({
        "frontend": fid,
        "active": fe.active_env,
        "origins": fe.origins,
        "label": fe.label,
        "rewrite_cookies": fe.rewrite_cookies,
        "rules": rules,
    }))
    .into_response()
}

pub async fn put_rules(
    State(state): State<AppState>,
    Query(q): Query<FrontendQuery>,
    Json(rules): Json<Vec<Rule>>,
) -> impl IntoResponse {
    let Some(fid) = q.frontend.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return bad("缺少 frontend 参数");
    };
    let mut config = state.snapshot_config().await;
    let Some(fe) = config.frontends.get_mut(fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    let active = fe.active_env.clone();
    let Some(env) = fe.envs.get_mut(&active) else {
        return bad(format!("环境 '{active}' 不存在"));
    };
    env.rules = rules;
    if let Err(e) = config.validate() {
        return bad(e);
    }
    if let Err(e) = save_apply(&state, &config) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response();
    }
    state.apply_config(&config).await;
    let rules = config
        .frontends
        .get(fid)
        .and_then(|f| f.envs.get(&f.active_env))
        .map(|e| e.rules.clone())
        .unwrap_or_default();
    Json(json!({ "ok": true, "frontend": fid, "active": active, "rules": rules })).into_response()
}

#[derive(Deserialize)]
pub struct LogsQuery {
    #[serde(default = "default_limit")]
    pub limit: usize,
    pub frontend: Option<String>,
    pub status_min: Option<u16>,
    pub status_max: Option<u16>,
}

fn default_limit() -> usize {
    50
}

pub async fn get_logs(
    State(state): State<AppState>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 200);
    let fe = q
        .frontend
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let logs = state
        .recent_logs(limit, fe, q.status_min, q.status_max)
        .await;
    Json(logs)
}

#[derive(Deserialize)]
pub struct ClearLogsBody {
    pub frontend: Option<String>,
}

pub async fn clear_logs(
    State(state): State<AppState>,
    Json(body): Json<ClearLogsBody>,
) -> impl IntoResponse {
    let fe = body
        .frontend
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    state.clear_logs(fe).await;
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
pub struct ProbeBody {
    pub frontend: String,
    /// 规则名；与 match_prefix 二选一
    #[serde(default)]
    pub rule_name: Option<String>,
    #[serde(default)]
    pub match_prefix: Option<String>,
    /// 探测路径，默认用 match_prefix
    #[serde(default)]
    pub path: Option<String>,
}

pub async fn probe_rule(
    State(state): State<AppState>,
    Json(body): Json<ProbeBody>,
) -> impl IntoResponse {
    use crate::proxy::{body_preview, build_upstream_url};
    use std::time::Instant;

    let fid = body.frontend.trim().to_string();
    let frontends = state.frontends.read().await;
    let Some(fe) = frontends.get(&fid) else {
        return bad(format!("前端 '{fid}' 不存在"));
    };
    let env_id = fe.active_env.clone();
    let Some(env) = fe.envs.get(&env_id) else {
        return bad(format!("环境 '{env_id}' 不存在"));
    };

    let rule = if let Some(name) = body
        .rule_name
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        env.rules.iter().find(|r| r.name == name).cloned()
    } else if let Some(prefix) = body
        .match_prefix
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        env.rules
            .iter()
            .find(|r| r.match_prefix == prefix)
            .cloned()
    } else {
        env.rules.first().cloned()
    };

    let Some(rule) = rule else {
        return bad("未找到可探测的规则");
    };

    let path = body
        .path
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| rule.match_prefix.clone());

    let url = build_upstream_url(
        &rule.target,
        &path,
        None,
        rule.strip_prefix,
        &rule.match_prefix,
    );
    drop(frontends);

    let started = Instant::now();
    let result = state.http.get(&url).send().await;
    let duration_ms = started.elapsed().as_millis() as u64;

    match result {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let ct = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            let bytes = resp.bytes().await.unwrap_or_default();
            let preview = body_preview(ct.as_deref(), &bytes);
            Json(json!({
                "ok": status < 500,
                "url": url,
                "status": status,
                "duration_ms": duration_ms,
                "rule": rule.name,
                "env": env_id,
                "frontend": fid,
                "body_preview": preview,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "ok": false,
            "url": url,
            "status": 0,
            "duration_ms": duration_ms,
            "rule": rule.name,
            "env": env_id,
            "frontend": fid,
            "error": format!("{e}"),
        }))
        .into_response(),
    }
}

pub async fn reload_config(State(state): State<AppState>) -> impl IntoResponse {
    match Config::load(state.config_path.as_str()) {
        Ok(config) => {
            state.apply_config(&config).await;
            Json(json!({ "ok": true })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response(),
    }
}
