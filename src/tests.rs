use crate::{
    build_app,
    config::{Config, Rule},
    providers::{config::Provider, ProviderRegistry},
    state::AppState,
};
use axum::{
    body::{to_bytes, Body},
    extract::{ConnectInfo, Request},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    state: AppState,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");
        let config = Config::default();
        config.save(&config_path).unwrap();
        let providers = ProviderRegistry::load(dir.path().join("providers.yaml")).unwrap();
        Self {
            state: AppState::from_config(&config, config_path.to_str().unwrap().into(), providers),
            _dir: dir,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:8787")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(
                "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            ))
            .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
            .unwrap();
        let response = build_app(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let data = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, data)
    }

    async fn provider(&self, base_url: &str) {
        self.state
            .providers
            .save(provider(base_url), true)
            .await
            .unwrap();
    }

    async fn chat(&self) -> (StatusCode, Value) {
        self.call("POST", "/__admin/api/playground/chat", Some(chat_body()))
            .await
    }
}

fn provider(base_url: &str) -> Provider {
    serde_json::from_value(
        json!({"id":"test", "name":"测试", "base_url":base_url, "models":["model-a"]}),
    )
    .unwrap()
}

fn chat_body() -> Value {
    json!({"provider_id":"test", "model":"model-a", "messages":[{"role":"system","content":"Be concise"},{"role":"user","content":"你好"}], "parameters":{"temperature":0.5,"max_completion_tokens":32}})
}

struct MockServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(app: Router) -> MockServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockServer { url, task }
}

#[tokio::test]
async fn provider_crud_persists_without_changing_proxy_config() {
    let f = Fixture::new();
    let before = std::fs::read(f.state.config_path.as_str()).unwrap();
    let body = serde_json::to_value(provider("https://example.com/v1/")).unwrap();
    let (status, created) = f
        .call("POST", "/__admin/api/providers", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["base_url"], "https://example.com/v1");
    assert_eq!(created["protocol"], "openai_compatible");
    assert_eq!(
        f.call("POST", "/__admin/api/providers", Some(body.clone()))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        f.call("GET", "/__admin/api/providers/test", None).await.1["id"],
        "test"
    );
    let mut updated = body;
    updated["name"] = json!("更新的供应商");
    assert_eq!(
        f.call("PUT", "/__admin/api/providers/test", Some(updated))
            .await
            .0,
        StatusCode::OK
    );
    let reloaded = ProviderRegistry::load(f._dir.path().join("providers.yaml")).unwrap();
    assert_eq!(reloaded.get("test").await.unwrap().name, "更新的供应商");
    assert_eq!(
        f.call("GET", "/__admin/api/providers", None).await.1["providers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(std::fs::read(f.state.config_path.as_str()).unwrap(), before);
    assert_eq!(
        f.call("DELETE", "/__admin/api/providers/test", None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        f.call("GET", "/__admin/api/providers/test", None).await.0,
        StatusCode::NOT_FOUND
    );
    assert!(ProviderRegistry::load(f._dir.path().join("providers.yaml"))
        .unwrap()
        .list()
        .await
        .is_empty());
}

#[tokio::test]
async fn registry_serializes_concurrent_writes_and_rolls_back_failed_writes() {
    let f = Fixture::new();
    let mut tasks = vec![];
    for i in 0..12 {
        let registry = f.state.providers.clone();
        tasks.push(tokio::spawn(async move {
            let mut p = provider("https://example.com/v1");
            p.id = format!("p{i}");
            registry.save(p, true).await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(f.state.providers.list().await.len(), 12);
    assert_eq!(
        ProviderRegistry::load(f._dir.path().join("providers.yaml"))
            .unwrap()
            .list()
            .await
            .len(),
        12
    );
    let registry =
        ProviderRegistry::load(f._dir.path().join("missing-parent/providers.yaml")).unwrap();
    assert!(registry
        .save(provider("https://example.com"), true)
        .await
        .is_err());
    assert!(registry.list().await.is_empty());
}

#[test]
fn provider_validation_enforces_secret_references_and_safe_urls() {
    for url in [
        "file:///secret",
        "http://example.com/v1",
        "https://user:password@example.com",
        "https://example.com?key=secret",
        "https://example.com#key",
    ] {
        assert!(provider(url).validate().is_err(), "{url}");
    }
    for url in [
        "http://localhost:11434/v1",
        "http://127.0.0.1:11434/v1",
        "http://[::1]:11434/v1",
        "https://example.com/v1",
    ] {
        assert!(provider(url).validate().is_ok(), "{url}");
    }
    let mut p = provider("https://example.com/v1");
    for name in [
        "PATH",
        "AWS_SECRET_ACCESS_KEY",
        "RUST_API_KEY_",
        "RUST_API_KEY_bad",
    ] {
        p.api_key_env = Some(name.into());
        assert!(p.validate().is_err());
    }
    p.api_key_env = Some("RUST_API_KEY_TEST".into());
    assert!(p.validate().is_ok());
    assert!(serde_json::from_value::<Provider>(json!({"id":"p","name":"p","base_url":"https://example.com","models":["m"],"api_key":"secret"})).is_err());
}

#[tokio::test]
async fn successful_chat_forwards_payload_and_preserves_actual_usage() {
    let upstream = mock(Router::new().route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
        assert_eq!(body["model"], "model-a"); assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][1]["content"], "你好"); assert_eq!(body["temperature"], 0.5);
        Json(json!({"id":"call-1","choices":[{"message":{"content":"你好，开发者"}}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}))
    }))).await;
    let f = Fixture::new();
    f.provider(&format!("{}/v1", upstream.url)).await;
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["ok"], true);
    assert_eq!(data["http_status"], 200);
    assert_eq!(data["usage"]["total_tokens"], 10);
    assert_eq!(data["response"]["id"], "call-1");
    assert!(data["duration_ms"].is_u64());
    assert!(f.state.logs.read().await.is_empty());
}

#[tokio::test]
async fn credentials_are_server_side_and_redacted_even_when_echoed() {
    const NAME: &str = "RUST_API_KEY_TEST_AUTH_REDACTION";
    const SECRET: &str = "sk-synthetic-never-real-123";
    std::env::set_var(NAME, SECRET);
    let upstream = mock(Router::new().route("/chat/completions", post(|request: Request| async move {
        assert_eq!(request.headers()[header::AUTHORIZATION], format!("Bearer {SECRET}"));
        Json(json!({"choices":[{"message":{"content":format!("echo {SECRET}")}}], "debug":{"Authorization":"Bearer something-else","api_key":"other-key",SECRET:SECRET}}))
    }))).await;
    let f = Fixture::new();
    let mut p = provider(&upstream.url);
    p.api_key_env = Some(NAME.into());
    f.state.providers.save(p, true).await.unwrap();
    let (status, data) = f.chat().await;
    std::env::remove_var(NAME);
    assert_eq!(status, StatusCode::OK);
    assert!(!data.to_string().contains(SECRET));
    assert_eq!(data["response"]["debug"]["Authorization"], "[REDACTED]");
    let stored = std::fs::read_to_string(f._dir.path().join("providers.yaml")).unwrap();
    assert!(!stored.contains(SECRET));
    assert!(stored.contains(NAME));
    assert!(!f
        .call("GET", "/__admin/api/providers", None)
        .await
        .1
        .to_string()
        .contains(SECRET));
}

#[tokio::test]
async fn upstream_errors_keep_http_status_and_error_code() {
    for (code, kind) in [
        (401, "authentication"),
        (403, "authentication"),
        (429, "rate_limit"),
        (500, "upstream_server"),
        (400, "upstream_request"),
    ] {
        let upstream = mock(Router::new().fallback(move || async move {
            (
                StatusCode::from_u16(code).unwrap(),
                Json(json!({"error":{"code":"synthetic_error","message":"failure"}})),
            )
        }))
        .await;
        let f = Fixture::new();
        f.provider(&upstream.url).await;
        let (status, data) = f.chat().await;
        assert_eq!(status.as_u16(), code);
        assert_eq!(data["http_status"], code);
        assert_eq!(data["error"]["kind"], kind);
        assert_eq!(data["error"]["code"], "synthetic_error");
    }
}

#[tokio::test]
async fn invalid_json_schema_and_redirects_are_reported() {
    for text in [
        "not json",
        "null",
        "{}",
        "{\"choices\":[]}",
        "{\"choices\":[{}]}",
        "{\"error\":{\"message\":\"error in 200\"}}",
    ] {
        let text = text.to_string();
        let upstream = mock(Router::new().fallback(move || {
            let text = text.clone();
            async move { text }
        }))
        .await;
        let f = Fixture::new();
        f.provider(&upstream.url).await;
        let (status, data) = f.chat().await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(data["error"]["kind"], "protocol");
        assert_eq!(data["http_status"], 200);
    }
    let upstream = mock(Router::new().fallback(|| async {
        (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, "http://127.0.0.1:1/do-not-follow")],
            "redirect",
        )
    }))
    .await;
    let f = Fixture::new();
    f.provider(&upstream.url).await;
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(data["http_status"], 307);
    assert_eq!(data["error"]["kind"], "redirect");
}

#[tokio::test]
async fn timeout_includes_wait_for_complete_response_body() {
    let upstream = mock(Router::new().fallback(|| async {
        let (mut sender, body) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            sender.write_all(b"{").await.ok();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        // A streaming body which sends headers immediately but never completes in time.
        axum::response::Response::new(Body::new(TestBody(body)))
    }))
    .await;
    let f = Fixture::new();
    let mut p = provider(&upstream.url);
    p.timeout_seconds = 1;
    f.state.providers.save(p, true).await.unwrap();
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(data["error"]["kind"], "timeout");
    assert_eq!(data["http_status"], 200);
    assert!(data["duration_ms"].as_u64().unwrap() >= 900);
}

struct TestBody(tokio::io::DuplexStream);
impl axum::body::HttpBody for TestBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        use tokio::io::AsyncRead;
        let mut bytes = [0u8; 1024];
        let mut buf = tokio::io::ReadBuf::new(&mut bytes);
        match std::pin::Pin::new(&mut self.0).poll_read(cx, &mut buf) {
            std::task::Poll::Pending => std::task::Poll::Pending,
            std::task::Poll::Ready(Err(error)) => std::task::Poll::Ready(Some(Err(error))),
            std::task::Poll::Ready(Ok(())) if buf.filled().is_empty() => {
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Some(Ok(
                http_body::Frame::data(bytes::Bytes::copy_from_slice(buf.filled())),
            ))),
        }
    }
}

#[tokio::test]
async fn malformed_and_oversized_admin_requests_are_rejected() {
    let f = Fixture::new();
    for (text, expected) in [
        ("{broken".to_string(), StatusCode::BAD_REQUEST),
        (" ".repeat(256 * 1024 + 1), StatusCode::PAYLOAD_TOO_LARGE),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("/__admin/api/playground/chat")
            .header(header::HOST, "127.0.0.1:8787")
            .header(header::ORIGIN, "http://127.0.0.1:8787")
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()))
            .body(Body::from(text))
            .unwrap();
        let response = build_app(f.state.clone()).oneshot(req).await.unwrap();
        assert_eq!(response.status(), expected);
    }
}

#[tokio::test]
async fn request_validation_and_missing_credentials_do_not_call_upstream() {
    let f = Fixture::new();
    assert_eq!(f.chat().await.0, StatusCode::NOT_FOUND);
    f.provider("http://127.0.0.1:1").await;
    for (key, value) in [
        ("stream", json!(true)),
        ("model", json!("other")),
        ("temperature", json!(3)),
        ("max_tokens", json!(-1)),
    ] {
        let mut body = chat_body();
        body["parameters"][key] = value;
        assert_eq!(
            f.call("POST", "/__admin/api/playground/chat", Some(body))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut body = chat_body();
    body["model"] = json!("unknown");
    assert_eq!(
        f.call("POST", "/__admin/api/playground/chat", Some(body))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut p = provider("http://127.0.0.1:1");
    p.api_key_env = Some("RUST_API_KEY_UNSET_TEST_918273645".into());
    f.state.providers.save(p, false).await.unwrap();
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(data["error"]["kind"], "missing_credentials");
}

#[tokio::test]
async fn response_size_and_transport_errors_are_bounded() {
    let upstream = mock(Router::new().fallback(|| async { "x".repeat(2 * 1024 * 1024 + 1) })).await;
    let f = Fixture::new();
    f.provider(&upstream.url).await;
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(data["error"]["kind"], "response_too_large");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let reset = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(socket);
    });
    let mut p = provider(&url);
    p.timeout_seconds = 5;
    f.state.providers.save(p, false).await.unwrap();
    let (status, data) = f.chat().await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(data["error"]["kind"], "connection");
    assert_eq!(data["http_status"], Value::Null);
    reset.await.unwrap();
}

#[tokio::test]
async fn admin_access_rejects_remote_peers_cross_origin_and_dns_rebinding() {
    let f = Fixture::new();
    for (peer, host, origin) in [
        ("192.0.2.1:1", "127.0.0.1:8787", None),
        ("127.0.0.1:1", "attacker.test", None),
        (
            "127.0.0.1:1",
            "127.0.0.1:8787",
            Some("http://localhost:8080"),
        ),
        ("127.0.0.1:1", "127.0.0.1:8787", Some("null")),
    ] {
        let mut req = Request::builder()
            .uri("/__admin/api/providers")
            .header(header::HOST, host)
            .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        if let Some(origin) = origin {
            req = req.header(header::ORIGIN, origin);
        }
        let response = build_app(f.state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(
        f.call("GET", "/__admin/api/providers", None).await.0,
        StatusCode::OK
    );
    assert_eq!(f.call("GET", "/health", None).await.0, StatusCode::OK);
}

#[tokio::test]
async fn proxy_regression_preserves_rewrite_headers_cookie_cors_and_routing() {
    let upstream = mock(Router::new().fallback(|req: Request| async move {
        let uri = req.uri().to_string();
        let injected = req.headers()["x-test"].to_str().unwrap().to_string();
        let body = to_bytes(req.into_body(), 1024).await.unwrap();
        let mut response = (
            StatusCode::ACCEPTED,
            Json(json!({"uri":uri,"injected":injected,"body":String::from_utf8_lossy(&body)})),
        )
            .into_response();
        response.headers_mut().append(
            header::SET_COOKIE,
            "a=1; Domain=example.com; Secure; HttpOnly; Path=/"
                .parse()
                .unwrap(),
        );
        response
            .headers_mut()
            .append(header::SET_COOKIE, "b=2; Path=/".parse().unwrap());
        response
    }))
    .await;
    let f = Fixture::new();
    {
        let mut frontends = f.state.frontends.write().await;
        let fe = frontends.get_mut("default").unwrap();
        fe.rewrite_cookies = true;
        fe.envs.get_mut("default").unwrap().rules = vec![
            Rule {
                name: "short".into(),
                match_prefix: "/api".into(),
                target: "http://127.0.0.1:1".into(),
                strip_prefix: true,
                headers: HashMap::new(),
            },
            Rule {
                name: "long".into(),
                match_prefix: "/api/chat".into(),
                target: upstream.url.clone(),
                strip_prefix: true,
                headers: HashMap::from([("x-test".into(), "injected".into())]),
            },
        ];
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/chat/echo?q=hello%20world")
        .header(header::ORIGIN, "http://localhost:5173")
        .body(Body::from("original body"))
        .unwrap();
    let response = build_app(f.state.clone()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:5173"
    );
    assert_eq!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .count(),
        2
    );
    let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(!cookie.contains("Domain"));
    assert!(!cookie.contains("Secure"));
    assert!(cookie.contains("HttpOnly"));
    let data: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(data["uri"], "/echo?q=hello%20world");
    assert_eq!(data["injected"], "injected");
    assert_eq!(data["body"], "original body");
    assert_eq!(f.state.logs.read().await.back().unwrap().rule, "long");
    assert!(matches!(
        f.state
            .resolve(Some("http://localhost:5173"), "/apix")
            .await,
        Err(crate::state::ResolveError::NoRule(..))
    ));
}
