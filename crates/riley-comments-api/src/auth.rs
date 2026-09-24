use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
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
    pub fn user_id(&self) -> Result<Uuid, uuid::Error> {
        self.sub.parse()
    }

    /// Whether the token *claims* the admin role. Access tokens are long-lived,
    /// so this can be stale: gate admin actions on [`RoleChecker::require_admin`].
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

/// The raw access token a request authenticated with. Inserted by
/// [`require_auth`] so admin actions can re-check the caller's role live.
#[derive(Clone)]
pub struct AccessToken(String);

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

// ── Live role check ──────────────────────────────────────────────────

/// Confirms admin rights against riley-auth's `GET /auth/me`, which reads the
/// user's current role from its database.
///
/// Access tokens live for weeks (30 days in production), so the `role` claim
/// can outlive a demotion. Admin-only actions must call [`Self::require_admin`];
/// ordinary user actions stay on the JWT alone.
pub struct RoleChecker {
    client: reqwest::Client,
    me_url: String,
}

#[derive(Deserialize)]
struct MeResponse {
    id: String,
    role: String,
}

impl RoleChecker {
    /// `me_url` is the full URL of riley-auth's `/auth/me` endpoint.
    pub fn new(me_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client builds with static options");
        Self { client, me_url }
    }

    /// Derive the `/auth/me` URL from the configured JWKS URL, which is served
    /// by the same riley-auth instance: `{base}/.well-known/jwks.json` maps to
    /// `{base}/auth/me`. Any other JWKS path falls back to the URL's origin.
    pub fn from_jwks_url(jwks_url: &str) -> anyhow::Result<Self> {
        Ok(Self::new(me_url_from_jwks_url(jwks_url)?))
    }

    pub fn me_url(&self) -> &str {
        &self.me_url
    }

    /// Succeed only if riley-auth confirms, right now, that the token's user
    /// is an admin. Fails closed: a stale or non-admin role is `Forbidden`,
    /// and an unreachable or misbehaving riley-auth is `Unavailable`.
    pub async fn require_admin(
        &self,
        claims: &Claims,
        token: &AccessToken,
    ) -> Result<(), riley_comments_core::Error> {
        use riley_comments_core::Error;

        let forbidden = || Error::Forbidden("admin privileges required".to_string());
        let unavailable = || Error::Unavailable("could not confirm admin role".to_string());

        // Tokens that don't even claim admin never get a network round-trip.
        if !claims.is_admin() {
            return Err(forbidden());
        }

        let resp = self
            .client
            .get(&self.me_url)
            .header(reqwest::header::COOKIE, format!("auth_access={}", token.0))
            .send()
            .await
            .map_err(|e| {
                tracing::warn!("admin role check: riley-auth request failed: {e}");
                unavailable()
            })?;

        let status = resp.status();
        if status.is_client_error() && status != reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Token no longer accepted, or the user is gone.
            tracing::info!(%status, sub = %claims.sub, "admin role check rejected by riley-auth");
            return Err(forbidden());
        }
        if !status.is_success() {
            tracing::warn!(%status, "admin role check: unexpected riley-auth status");
            return Err(unavailable());
        }

        let me: MeResponse = resp.json().await.map_err(|e| {
            tracing::warn!("admin role check: bad riley-auth response: {e}");
            unavailable()
        })?;

        if me.id != claims.sub {
            tracing::warn!(sub = %claims.sub, "admin role check: riley-auth returned a different user");
            return Err(forbidden());
        }
        if me.role != "admin" {
            tracing::info!(sub = %claims.sub, role = %me.role, "stale admin claim rejected");
            return Err(forbidden());
        }
        Ok(())
    }
}

fn me_url_from_jwks_url(jwks_url: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(jwks_url)
        .map_err(|e| anyhow::anyhow!("invalid auth.jwks_url {jwks_url:?}: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        anyhow::bail!("invalid auth.jwks_url {jwks_url:?}: expected an http(s) URL");
    }
    let base = url
        .path()
        .trim_end_matches('/')
        .strip_suffix("/.well-known/jwks.json")
        .unwrap_or("")
        .to_string();
    url.set_path(&format!("{base}/auth/me"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
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
                DecodingKey::from_rsa_components(n, e)
                    .ok()
                    .map(|k| (k, alg))
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

    /// A cache preloaded with fixed keys, for tests.
    #[cfg(test)]
    pub(crate) fn with_keys(
        keys: Vec<(DecodingKey, Algorithm)>,
        expected_issuer: Option<String>,
        expected_audience: Option<String>,
    ) -> Self {
        Self {
            url: String::new(),
            client: reqwest::Client::new(),
            keys: RwLock::new(keys),
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
            return Err(error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth keys not loaded",
            ));
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

        Err(error_response(
            StatusCode::UNAUTHORIZED,
            "invalid or expired token",
        ))
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
pub async fn require_auth(request: Request, next: Next) -> Result<Response, Response> {
    let jwks = request
        .extensions()
        .get::<Arc<JwksCache>>()
        .cloned()
        .ok_or_else(|| error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth not configured"))?;

    let token = extract_token(&request)
        .ok_or_else(|| error_response(StatusCode::UNAUTHORIZED, "missing authorization header"))?;

    let claims = jwks.verify(&token).await?;
    let mut request = request;
    request.extensions_mut().insert(claims);
    request.extensions_mut().insert(AccessToken(token));

    Ok(next.run(request).await)
}

/// Middleware that optionally attaches Claims if a valid token is present.
/// Does not reject unauthenticated requests.
pub async fn optional_auth(request: Request, next: Next) -> Response {
    let jwks = request.extensions().get::<Arc<JwksCache>>().cloned();

    if let Some(jwks) = jwks
        && let Some(token) = extract_token(&request)
        && let Ok(claims) = jwks.verify(&token).await
    {
        let mut request = request;
        request.extensions_mut().insert(claims);
        return next.run(request).await;
    }

    next.run(request).await
}

fn extract_token(request: &Request) -> Option<String> {
    // Try Authorization: Bearer header first
    if let Some(token) = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(token.to_string());
    }

    // Fall back to auth_access cookie
    request
        .headers()
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .map(|s| s.trim())
                .find(|s| s.starts_with("auth_access="))
                .and_then(|s| s.strip_prefix("auth_access="))
                .map(|s| s.to_string())
        })
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({"error": message}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn me_url_derived_from_jwks_url() {
        let cases = [
            (
                "http://riley-auth:8081/.well-known/jwks.json",
                "http://riley-auth:8081/auth/me",
            ),
            (
                "https://auth.rileyleff.com/.well-known/jwks.json",
                "https://auth.rileyleff.com/auth/me",
            ),
            (
                "https://example.com/riley-auth/.well-known/jwks.json/",
                "https://example.com/riley-auth/auth/me",
            ),
            (
                "http://riley-auth:8081/keys.json?x=1#y",
                "http://riley-auth:8081/auth/me",
            ),
        ];
        for (jwks, me) in cases {
            assert_eq!(me_url_from_jwks_url(jwks).unwrap(), me, "{jwks}");
        }
        assert!(me_url_from_jwks_url("riley-auth:8081/jwks").is_err());
        assert!(me_url_from_jwks_url("file:///etc/jwks.json").is_err());
    }

    #[test]
    fn access_token_debug_is_redacted() {
        let t = AccessToken("secret.jwt.value".to_string());
        assert!(!format!("{t:?}").contains("secret"));
    }
}
