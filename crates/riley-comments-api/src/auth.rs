use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

// ── JWT Claims ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub username: String,
    pub role: String,
    #[serde(default)]
    pub aud: Option<String>,
    pub iss: Option<String>,
    pub iat: Option<i64>,
    pub exp: i64,
}

impl Claims {
    pub fn user_id(&self) -> Result<Uuid, Response> {
        self.sub.parse().map_err(|_| {
            (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "invalid user id in token"}))).into_response()
        })
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

// ── JWKS Cache ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

#[derive(Debug, Clone, Deserialize)]
struct JwkKey {
    kty: String,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    use_: Option<String>,
}

impl JwkKey {
    fn to_decoding_key(&self) -> Option<(DecodingKey, Algorithm)> {
        match self.kty.as_str() {
            "EC" => {
                let crv = self.crv.as_deref()?;
                let x = self.x.as_deref()?;
                let y = self.y.as_deref()?;
                let alg = match crv {
                    "P-256" => Algorithm::ES256,
                    "P-384" => Algorithm::ES384,
                    _ => return None,
                };
                DecodingKey::from_ec_components(x, y).ok().map(|k| (k, alg))
            }
            "RSA" => {
                let n = self.n.as_deref()?;
                let e = self.e.as_deref()?;
                let alg = match self.alg.as_deref() {
                    Some("RS384") => Algorithm::RS384,
                    Some("RS512") => Algorithm::RS512,
                    _ => Algorithm::RS256,
                };
                DecodingKey::from_rsa_components(n, e).ok().map(|k| (k, alg))
            }
            _ => None,
        }
    }
}

pub struct JwksCache {
    url: String,
    client: reqwest::Client,
    keys: RwLock<Vec<(DecodingKey, Algorithm)>>,
    expected_issuer: Option<String>,
    expected_audience: Option<String>,
}

impl JwksCache {
    pub fn new(
        url: String,
        expected_issuer: Option<String>,
        expected_audience: Option<String>,
    ) -> Self {
        Self {
            url,
            client: reqwest::Client::new(),
            keys: RwLock::new(Vec::new()),
            expected_issuer,
            expected_audience,
        }
    }

    pub async fn refresh(&self) -> anyhow::Result<()> {
        let resp: JwksResponse = self.client.get(&self.url).send().await?.json().await?;
        let keys: Vec<(DecodingKey, Algorithm)> = resp
            .keys
            .iter()
            .filter(|k| k.use_.as_deref() != Some("enc"))
            .filter_map(|k| k.to_decoding_key())
            .collect();

        if keys.is_empty() {
            anyhow::bail!("JWKS returned no usable signing keys");
        }

        let count = keys.len();
        *self.keys.write().await = keys;
        tracing::info!(count, "JWKS refreshed");
        Ok(())
    }

    pub async fn verify(&self, token: &str) -> Result<Claims, Response> {
        let keys = self.keys.read().await;
        if keys.is_empty() {
            return Err(error_response(StatusCode::SERVICE_UNAVAILABLE, "auth keys not loaded"));
        }

        for (key, alg) in keys.iter() {
            let mut validation = Validation::new(*alg);
            validation.validate_exp = true;

            if let Some(iss) = &self.expected_issuer {
                validation.set_issuer(&[iss]);
            } else {
                validation.iss = None;
            }

            if let Some(aud) = &self.expected_audience {
                validation.set_audience(&[aud]);
            } else {
                validation.validate_aud = false;
            }

            match decode::<Claims>(token, key, &validation) {
                Ok(data) => return Ok(data.claims),
                Err(_) => continue,
            }
        }

        Err(error_response(StatusCode::UNAUTHORIZED, "invalid or expired token"))
    }

    /// Spawn a background task that refreshes JWKS periodically.
    pub fn spawn_refresh_task(self: &Arc<Self>) {
        let cache = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if let Err(e) = cache.refresh().await {
                    tracing::warn!("JWKS refresh failed: {e}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
    }
}

// ── Auth Middleware ───────────────────────────────────────────────────

/// Middleware that requires a valid JWT. Inserts Claims into request extensions.
pub async fn require_auth(
    request: Request,
    next: Next,
) -> Result<Response, Response> {
    let jwks = request
        .extensions()
        .get::<Arc<JwksCache>>()
        .cloned()
        .ok_or_else(|| error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth not configured"))?;

    let token = extract_bearer(&request)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "missing authorization header"))?;

    let claims = jwks.verify(&token).await?;
    let mut request = request;
    request.extensions_mut().insert(claims);

    Ok(next.run(request).await)
}

/// Middleware that optionally attaches Claims if a valid token is present.
/// Does not reject unauthenticated requests.
pub async fn optional_auth(
    request: Request,
    next: Next,
) -> Response {
    let jwks = request.extensions().get::<Arc<JwksCache>>().cloned();

    if let Some(jwks) = jwks {
        if let Some(token) = extract_bearer(&request) {
            if let Ok(claims) = jwks.verify(&token).await {
                let mut request = request;
                request.extensions_mut().insert(claims);
                return next.run(request).await;
            }
        }
    }

    next.run(request).await
}

fn extract_bearer(request: &Request) -> Option<String> {
    request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}
