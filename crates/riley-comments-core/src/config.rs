use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub comments: CommentsConfig,
    pub r2: Option<R2Config>,
    pub notifications: Option<NotificationsConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub cors_origins: Vec<String>,
    #[serde(default)]
    pub behind_proxy: bool,
    /// Origins allowed to make state-changing (POST/PUT/PATCH/DELETE) requests,
    /// checked against the Origin header (Referer as fallback) for CSRF protection,
    /// e.g. `["https://rileyleff.com"]`. If unset, falls back to `cors_origins`
    /// (ignoring `"*"`).
    #[serde(default)]
    pub csrf_origins: Option<Vec<String>>,
}

impl ServerConfig {
    /// Origins allowed to make state-changing requests (see `csrf_origins`).
    pub fn effective_csrf_origins(&self) -> Vec<String> {
        match &self.csrf_origins {
            Some(origins) => origins.clone(),
            None => self
                .cors_origins
                .iter()
                .filter(|o| o.as_str() != "*")
                .cloned()
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseConfig {
    pub url: ConfigValue,
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    pub jwks_url: String,
    pub expected_issuer: Option<String>,
    pub expected_audience: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommentsConfig {
    #[serde(default = "default_max_depth")]
    pub max_depth: i32,
    #[serde(default = "default_max_body_length")]
    pub max_body_length: usize,
}

impl Default for CommentsConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            max_body_length: default_max_body_length(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct R2Config {
    pub endpoint: ConfigValue,
    pub bucket: String,
    pub access_key_id: ConfigValue,
    pub secret_access_key: ConfigValue,
    /// Public URL prefix for serving images (e.g., "https://emoji.rileyleff.com" or R2.dev URL)
    pub public_url: ConfigValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationsConfig {
    /// Base URL of the notifications service (e.g. "http://riley-notifications:8084")
    pub url: String,
    /// API token for service-to-service auth
    pub api_token: ConfigValue,
}

/// A config value that can be a direct string or an env var reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Direct(String),
}

impl ConfigValue {
    pub fn resolve(&self) -> crate::Result<String> {
        match self {
            Self::Direct(v) => {
                if let Some(name) = v.strip_prefix("env:") {
                    std::env::var(name)
                        .map_err(|_| crate::Error::Config(format!("env var not set: {name}")))
                } else {
                    Ok(v.clone())
                }
            }
        }
    }
}

pub fn load_config(path: &std::path::Path) -> crate::Result<Config> {
    let contents = std::fs::read_to_string(path)
        .map_err(|e| crate::Error::Config(format!("failed to read config: {e}")))?;
    toml::from_str(&contents)
        .map_err(|e| crate::Error::Config(format!("failed to parse config: {e}")))
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    8083
}
fn default_max_connections() -> u32 {
    5
}
fn default_max_depth() -> i32 {
    1
}
fn default_max_body_length() -> usize {
    10_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
[database]
url = "postgres://localhost/test"

[auth]
jwks_url = "http://riley-auth:8081/.well-known/jwks.json"
"#;

    fn parse(server: &str) -> Config {
        toml::from_str(&format!("[server]\n{server}\n{BASE}")).unwrap()
    }

    #[test]
    fn csrf_origins_explicit() {
        let c = parse(
            r#"cors_origins = ["https://a.example"]
csrf_origins = ["https://rileyleff.com"]"#,
        );
        assert_eq!(
            c.server.effective_csrf_origins(),
            vec!["https://rileyleff.com"]
        );
    }

    #[test]
    fn csrf_origins_fall_back_to_cors_without_wildcard() {
        let c = parse(r#"cors_origins = ["https://rileyleff.com", "*"]"#);
        assert_eq!(
            c.server.effective_csrf_origins(),
            vec!["https://rileyleff.com"]
        );

        let c = parse("");
        assert!(c.server.effective_csrf_origins().is_empty());
    }
}
