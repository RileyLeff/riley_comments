use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub auth: AuthConfig,
    #[serde(default)]
    pub comments: CommentsConfig,
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
                    std::env::var(name).map_err(|_| {
                        crate::Error::Config(format!("env var not set: {name}"))
                    })
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
    3
}
fn default_max_body_length() -> usize {
    10_000
}
