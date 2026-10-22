use serde_json::Value;

pub fn resolve(name: Option<&str>) -> Result<Option<String>, &'static str> {
    match name {
        None => Ok(None),
        Some(name) => std::env::var(name)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(Some)
            .ok_or("供应商密钥环境变量未设置；请在启动服务前设置该变量"),
    }
}

pub fn redact_text(text: &str, secret: Option<&str>) -> String {
    match secret.filter(|s| !s.is_empty()) {
        Some(secret) => text.replace(secret, "[REDACTED]"),
        None => text.to_string(),
    }
}

pub fn redact_json(value: &mut Value, secret: Option<&str>) {
    match value {
        Value::Object(map) => {
            // Redact keys as well, in case an upstream echoes credentials as object keys.
            let entries = std::mem::take(map);
            for (key, mut value) in entries {
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                if [
                    "authorization",
                    "api_key",
                    "apikey",
                    "access_token",
                    "refresh_token",
                    "client_secret",
                    "secret_key",
                    "x_api_key",
                    "proxy_authorization",
                    "password",
                    "cookie",
                    "set_cookie",
                ]
                .contains(&normalized.as_str())
                {
                    value = Value::String("[REDACTED]".into());
                } else {
                    redact_json(&mut value, secret);
                }
                map.insert(redact_text(&key, secret), value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|v| redact_json(v, secret)),
        Value::String(text) => *text = redact_text(text, secret),
        _ => {}
    }
}
