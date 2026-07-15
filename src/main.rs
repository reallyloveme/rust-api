mod admin;
mod config;
mod cors;
mod proxy;
mod state;

use admin::{
    admin_page, clear_logs, create_env, create_frontend, delete_env, delete_frontend, get_logs,
    get_rules, list_envs, list_frontends, probe_rule, put_rules, reload_config, set_active,
    update_frontend,
};
use axum::{
    extract::State,
    middleware,
    response::IntoResponse,
    routing::{any, get, post, put},
    Json, Router,
};
use config::Config;
use cors::dynamic_cors;
use proxy::proxy_handler;
use serde_json::json;
use state::AppState;
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let frontends = state.frontends.read().await;
    let mut ids: Vec<_> = frontends.keys().cloned().collect();
    ids.sort();
    Json(json!({
        "ok": true,
        "listen": state.listen.as_str(),
        "frontends": ids.len(),
        "frontend_ids": ids,
    }))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rust_api=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".into());
    let config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::warn!("{e}，使用默认配置");
        Config::default()
    });

    let listen = config.listen.clone();
    let fe_count = config.frontends.len();
    let state = AppState::from_config(&config, config_path);

    let admin_routes = Router::new()
        .route("/", get(admin_page))
        .route("/api/frontends", get(list_frontends).post(create_frontend))
        .route("/api/frontends/update", put(update_frontend))
        .route("/api/frontends/delete", post(delete_frontend))
        .route("/api/envs", get(list_envs).post(create_env))
        .route("/api/envs/active", put(set_active))
        .route("/api/envs/delete", post(delete_env))
        .route("/api/rules", get(get_rules).put(put_rules))
        .route("/api/logs", get(get_logs))
        .route("/api/logs/clear", post(clear_logs))
        .route("/api/probe", post(probe_rule))
        .route("/api/reload", post(reload_config));

    let app = Router::new()
        .route("/health", get(health))
        .nest("/__admin", admin_routes)
        .fallback(any(proxy_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            dynamic_cors,
        ))
        .with_state(state);

    let addr: SocketAddr = listen
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:8787".parse().unwrap());

    tracing::info!("转发服务监听 http://{addr}");
    tracing::info!("已配置 {fe_count} 个前端");
    tracing::info!("健康检查 http://{addr}/health");
    tracing::info!("管理台 http://{addr}/__admin");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("绑定 {addr} 失败: {e}"));

    axum::serve(listener, app).await.expect("服务异常退出");
}
