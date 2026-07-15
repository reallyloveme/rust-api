use crate::config::{Config, FrontendConfig, Rule};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;

const LOG_CAPACITY: usize = 500;

#[derive(Debug, Clone, Serialize)]
pub struct RequestLog {
    pub time: DateTime<Utc>,
    pub method: String,
    pub path: String,
    pub target: String,
    pub status: u16,
    pub duration_ms: u64,
    pub rule: String,
    pub env: String,
    pub frontend: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_preview: Option<String>,
}

#[derive(Clone)]
pub struct AppState {
    pub frontends: Arc<RwLock<HashMap<String, FrontendConfig>>>,
    pub logs: Arc<RwLock<VecDeque<RequestLog>>>,
    pub config_path: Arc<String>,
    pub cors_origins: Arc<RwLock<Vec<String>>>,
    pub listen: Arc<String>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn from_config(config: &Config, config_path: String) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("创建 HTTP 客户端失败");

        Self {
            frontends: Arc::new(RwLock::new(config.frontends.clone())),
            logs: Arc::new(RwLock::new(VecDeque::with_capacity(LOG_CAPACITY))),
            config_path: Arc::new(config_path),
            cors_origins: Arc::new(RwLock::new(config.all_origins())),
            listen: Arc::new(config.listen.clone()),
            http,
        }
    }

    pub async fn apply_config(&self, config: &Config) {
        *self.frontends.write().await = config.frontends.clone();
        *self.cors_origins.write().await = config.all_origins();
    }

    pub async fn snapshot_config(&self) -> Config {
        let frontends = self.frontends.read().await.clone();
        let mut config = Config {
            listen: self.listen.as_ref().clone(),
            cors: crate::config::CorsConfig {
                allow_origins: self.cors_origins.read().await.clone(),
            },
            frontends,
            active_env: String::new(),
            envs: HashMap::new(),
            rules: vec![],
        };
        let _ = config.normalize();
        config
    }

    pub async fn push_log(&self, entry: RequestLog) {
        let mut logs = self.logs.write().await;
        if logs.len() >= LOG_CAPACITY {
            logs.pop_front();
        }
        logs.push_back(entry);
    }

    pub async fn recent_logs(
        &self,
        limit: usize,
        frontend: Option<&str>,
        status_min: Option<u16>,
        status_max: Option<u16>,
    ) -> Vec<RequestLog> {
        let logs = self.logs.read().await;
        logs.iter()
            .rev()
            .filter(|l| frontend.map(|f| l.frontend == f).unwrap_or(true))
            .filter(|l| status_min.map(|m| l.status >= m).unwrap_or(true))
            .filter(|l| status_max.map(|m| l.status <= m).unwrap_or(true))
            .take(limit)
            .cloned()
            .collect()
    }

    pub async fn clear_logs(&self, frontend: Option<&str>) {
        let mut logs = self.logs.write().await;
        if let Some(f) = frontend {
            logs.retain(|l| l.frontend != f);
        } else {
            logs.clear();
        }
    }

    /// 按 Origin 找前端，再在其当前环境规则里最长前缀匹配
    pub async fn resolve(
        &self,
        origin: Option<&str>,
        path: &str,
    ) -> Result<(String, String, Rule), ResolveError> {
        let frontends = self.frontends.read().await;
        let (fid, fe) = match origin {
            Some(o) => {
                let o = o.trim().trim_end_matches('/');
                frontends
                    .iter()
                    .find(|(_, fe)| {
                        fe.origins
                            .iter()
                            .any(|x| x.trim().trim_end_matches('/') == o)
                    })
                    .map(|(id, fe)| (id.clone(), fe))
                    .ok_or(ResolveError::UnknownOrigin(o.to_string()))?
            }
            None => {
                // 无 Origin（如 curl）：若只有一个前端则用它，否则报错
                if frontends.len() == 1 {
                    let (id, fe) = frontends.iter().next().unwrap();
                    (id.clone(), fe)
                } else {
                    return Err(ResolveError::MissingOrigin);
                }
            }
        };

        let env_id = fe.active_env.clone();
        let rules = fe
            .envs
            .get(&env_id)
            .map(|e| &e.rules)
            .ok_or_else(|| ResolveError::NoEnv(fid.clone(), env_id.clone()))?;

        let mut best: Option<&Rule> = None;
        for rule in rules.iter() {
            if path_matches(path, &rule.match_prefix) {
                match best {
                    None => best = Some(rule),
                    Some(cur) if rule.match_prefix.len() > cur.match_prefix.len() => {
                        best = Some(rule);
                    }
                    _ => {}
                }
            }
        }
        let rule = best
            .cloned()
            .ok_or_else(|| ResolveError::NoRule(fid.clone(), env_id.clone(), path.to_string()))?;
        Ok((fid, env_id, rule))
    }
}

#[derive(Debug)]
pub enum ResolveError {
    MissingOrigin,
    UnknownOrigin(String),
    NoEnv(String, String),
    NoRule(String, String, String),
}

fn path_matches(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    if path == prefix {
        return true;
    }
    path.starts_with(prefix) && {
        let rest = &path[prefix.len()..];
        rest.is_empty() || rest.starts_with('/')
    }
}

pub fn extract_origin(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(o) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        let o = o.trim();
        if !o.is_empty() && o != "null" {
            return Some(o.trim_end_matches('/').to_string());
        }
    }
    if let Some(r) = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
    {
        return origin_from_url(r);
    }
    None
}

fn origin_from_url(url: &str) -> Option<String> {
    let (scheme, rest) = if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else {
        return None;
    };
    let hostport = rest.split('/').next()?.split('?').next()?;
    if hostport.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{hostport}"))
}
