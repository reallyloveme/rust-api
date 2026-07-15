use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub listen: String,
    #[serde(default)]
    pub cors: CorsConfig,
    /// 多前端：按 Origin 匹配，各自有独立环境与规则
    #[serde(default)]
    pub frontends: HashMap<String, FrontendConfig>,
    /// 兼容：旧版顶层 active_env / envs / rules
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_env: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub envs: HashMap<String, EnvConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrontendConfig {
    #[serde(default)]
    pub label: String,
    /// 匹配该前端的 Origin 列表，如 http://localhost:8080
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default = "default_active_env")]
    pub active_env: String,
    #[serde(default)]
    pub envs: HashMap<String, EnvConfig>,
    /// 改写上游 Set-Cookie：去掉 Domain，HTTP 源去掉 Secure，便于本地带登录态
    #[serde(default)]
    pub rewrite_cookies: bool,
}

fn default_active_env() -> String {
    "default".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvConfig {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorsConfig {
    #[serde(default = "default_origins")]
    pub allow_origins: Vec<String>,
}

fn default_origins() -> Vec<String> {
    vec![
        "http://localhost:5173".into(),
        "http://127.0.0.1:5173".into(),
        "http://localhost:3000".into(),
        "http://127.0.0.1:3000".into(),
        "http://localhost:8080".into(),
    ]
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: default_origins(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub match_prefix: String,
    pub target: String,
    #[serde(default)]
    pub strip_prefix: bool,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .map_err(|e| format!("读取配置失败 {}: {}", path.display(), e))?;
        let mut config: Config = serde_yaml::from_str(&content)
            .map_err(|e| format!("解析配置失败 {}: {}", path.display(), e))?;
        config.normalize()?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let mut out = self.clone();
        out.active_env.clear();
        out.envs.clear();
        out.rules.clear();
        // CORS 合并各前端 origins，便于持久化
        out.cors.allow_origins = out.all_origins();
        let content = serde_yaml::to_string(&out)
            .map_err(|e| format!("序列化配置失败: {}", e))?;
        fs::write(path, content).map_err(|e| format!("写入配置失败 {}: {}", path.display(), e))?;
        Ok(())
    }

    pub fn all_origins(&self) -> Vec<String> {
        let mut set = Vec::new();
        for o in &self.cors.allow_origins {
            let t = o.trim().to_string();
            if !t.is_empty() && !set.contains(&t) {
                set.push(t);
            }
        }
        for fe in self.frontends.values() {
            for o in &fe.origins {
                let t = o.trim().to_string();
                if !t.is_empty() && !set.contains(&t) {
                    set.push(t);
                }
            }
        }
        set
    }

    pub fn normalize(&mut self) -> Result<(), String> {
        // 旧版：只有 envs/rules，迁到 default 前端
        if self.frontends.is_empty() {
            let mut envs = std::mem::take(&mut self.envs);
            let rules = std::mem::take(&mut self.rules);
            let active = if self.active_env.trim().is_empty() {
                "default".to_string()
            } else {
                self.active_env.clone()
            };
            if envs.is_empty() {
                envs.insert(
                    active.clone(),
                    EnvConfig {
                        label: "默认".into(),
                        rules,
                    },
                );
            }
            let origins = self.cors.allow_origins.clone();
            self.frontends.insert(
                "default".into(),
                FrontendConfig {
                    label: "默认前端".into(),
                    origins,
                    active_env: active,
                    envs,
                    rewrite_cookies: false,
                },
            );
            self.active_env.clear();
        }

        for (fid, fe) in self.frontends.iter_mut() {
            if fe.label.trim().is_empty() {
                fe.label = fid.clone();
            }
            if fe.envs.is_empty() {
                fe.envs.insert(
                    "default".into(),
                    EnvConfig {
                        label: "默认".into(),
                        rules: vec![],
                    },
                );
            }
            if fe.active_env.trim().is_empty() || !fe.envs.contains_key(&fe.active_env) {
                fe.active_env = fe
                    .envs
                    .keys()
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("前端 '{fid}' 的 envs 不能为空"))?;
            }
            for (eid, env) in fe.envs.iter_mut() {
                if env.label.trim().is_empty() {
                    env.label = eid.clone();
                }
            }
            for o in &mut fe.origins {
                *o = o.trim().trim_end_matches('/').to_string();
            }
        }
        Ok(())
    }

    pub fn validate(&mut self) -> Result<(), String> {
        if self.listen.trim().is_empty() {
            return Err("listen 不能为空".into());
        }
        if self.frontends.is_empty() {
            return Err("至少需要一个前端".into());
        }
        for (fid, fe) in &mut self.frontends {
            if fe.envs.is_empty() {
                return Err(format!("前端 '{fid}' 至少需要一个环境"));
            }
            if !fe.envs.contains_key(&fe.active_env) {
                return Err(format!(
                    "前端 '{fid}' 的 active_env '{}' 不存在",
                    fe.active_env
                ));
            }
            for (eid, env) in &mut fe.envs {
                for rule in &mut env.rules {
                    if rule.name.trim().is_empty() {
                        return Err(format!("前端 '{fid}' / 环境 '{eid}' 规则 name 不能为空"));
                    }
                    if !rule.match_prefix.starts_with('/') {
                        return Err(format!(
                            "前端 '{fid}' / 环境 '{eid}' 规则 '{}' 的 match_prefix 必须以 / 开头",
                            rule.name
                        ));
                    }
                    if rule.target.trim().is_empty() {
                        return Err(format!(
                            "前端 '{fid}' / 环境 '{eid}' 规则 '{}' 的 target 不能为空",
                            rule.name
                        ));
                    }
                    while rule.target.ends_with('/') {
                        rule.target.pop();
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for Config {
    fn default() -> Self {
        let mut envs = HashMap::new();
        envs.insert(
            "default".into(),
            EnvConfig {
                label: "默认".into(),
                rules: vec![],
            },
        );
        let mut frontends = HashMap::new();
        frontends.insert(
            "default".into(),
            FrontendConfig {
                label: "默认前端".into(),
                origins: default_origins(),
                active_env: "default".into(),
                envs,
                rewrite_cookies: false,
            },
        );
        Self {
            listen: "127.0.0.1:8787".into(),
            cors: Default::default(),
            frontends,
            active_env: String::new(),
            envs: HashMap::new(),
            rules: vec![],
        }
    }
}
