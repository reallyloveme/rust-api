use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    OpenaiCompatible,
}

/// Only a reference to a server-side environment variable; never an API key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub protocol: Protocol,
    pub models: Vec<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    60
}

impl Provider {
    pub fn validate(&mut self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("供应商 id 须为 1–64 位字母、数字、下划线或连字符".into());
        }
        self.name = self.name.trim().to_string();
        if self.name.is_empty() || self.name.len() > 200 {
            return Err("供应商名称须为 1–200 字节".into());
        }
        let mut url =
            reqwest::Url::parse(self.base_url.trim()).map_err(|_| "Base URL 无效".to_string())?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Base URL 不允许包含凭证、查询参数或片段".into());
        }
        let local = url.host_str().is_some_and(|h| {
            h.eq_ignore_ascii_case("localhost")
                || h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if url.host_str().is_none()
            || !(url.scheme() == "https" || (url.scheme() == "http" && local))
        {
            return Err("Base URL 必须使用 HTTPS；本机模型服务可使用 HTTP".into());
        }
        let path = url.path().trim_end_matches('/').to_string();
        url.set_path(&path);
        self.base_url = url.as_str().trim_end_matches('/').to_string();
        if self.models.is_empty() || self.models.len() > 100 {
            return Err("请配置 1–100 个模型名称".into());
        }
        for model in &mut self.models {
            *model = model.trim().to_string();
            if model.is_empty() || model.len() > 200 {
                return Err("模型名称须为 1–200 字节".into());
            }
        }
        self.models.sort();
        self.models.dedup();
        if !(1..=300).contains(&self.timeout_seconds) {
            return Err("超时须为 1–300 秒".into());
        }
        if let Some(name) = &self.api_key_env {
            if !name.starts_with("RUST_API_KEY_")
                || name.len() <= "RUST_API_KEY_".len()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err("密钥变量须以 RUST_API_KEY_ 开头，仅包含大写字母、数字和下划线".into());
            }
        }
        Ok(())
    }

    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}
