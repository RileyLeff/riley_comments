//! CSRF protection for state-changing requests.
//!
//! Auth cookies are scoped to `.rileyleff.com`, so every sibling subdomain
//! (e.g. forestroyale.rileyleff.com) is "same-site" and the browser attaches
//! the cookies to requests it makes to this API. SameSite=Lax does not help
//! there, and endpoints such as `POST /comments/{id}/delete` (no body) or the
//! multipart emoji upload are "simple" requests that skip CORS preflight.
//!
//! This middleware therefore requires every unsafe request (anything other
//! than GET/HEAD/OPTIONS/TRACE) to carry an `Origin` header — or, failing
//! that, a `Referer` — whose origin is in the configured allow-list.
//!
//! A request with neither header is only accepted if it authenticates with an
//! explicit `Authorization: Bearer` header. Browsers always send `Origin` on
//! cross-origin unsafe requests, and when a Bearer header is present the auth
//! middleware never falls back to the cookie, so such a request cannot be
//! riding on ambient browser credentials.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CsrfGuard {
    allowed: Arc<Vec<String>>,
}

impl CsrfGuard {
    /// Build a guard from configured origins (e.g. `https://rileyleff.com`).
    /// Fails if any configured origin is not a valid http(s) origin.
    pub fn new(origins: &[String]) -> anyhow::Result<Self> {
        let allowed = origins
            .iter()
            .map(|o| {
                normalize_origin(o)
                    .ok_or_else(|| anyhow::anyhow!("invalid CSRF allowed origin: {o:?}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if allowed.is_empty() {
            tracing::warn!(
                "no CSRF allowed origins configured; all browser write requests will be rejected"
            );
        }
        Ok(Self {
            allowed: Arc::new(allowed),
        })
    }

    fn is_allowed(&self, value: &str) -> bool {
        normalize_origin(value).is_some_and(|o| self.allowed.contains(&o))
    }

    /// Decide whether a request may proceed.
    pub fn check(&self, method: &Method, headers: &HeaderMap) -> Result<(), &'static str> {
        if is_safe_method(method) {
            return Ok(());
        }

        if let Some(origin) = headers.get(header::ORIGIN) {
            let origin = origin.to_str().map_err(|_| "invalid origin")?;
            return if self.is_allowed(origin) {
                Ok(())
            } else {
                Err("origin not allowed")
            };
        }

        if let Some(referer) = headers.get(header::REFERER) {
            let referer = referer.to_str().map_err(|_| "invalid referer")?;
            return if self.is_allowed(referer) {
                Ok(())
            } else {
                Err("referer not allowed")
            };
        }

        // No Origin and no Referer: not a browser-initiated cross-site request.
        // Only allow it if it authenticates explicitly (never via cookie).
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer "));
        if bearer {
            Ok(())
        } else {
            Err("missing origin")
        }
    }
}

fn is_safe_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}

/// Reduce an Origin or Referer value to `scheme://host[:port]`, lowercased,
/// with the default port removed. Returns `None` for anything that is not a
/// plain http(s) URL (including the literal `null` origin).
pub fn normalize_origin(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let (scheme, rest) = value.split_once("://")?;
    let default_port = match scheme {
        "https" => ":443",
        "http" => ":80",
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty()
        || authority
            .chars()
            .any(|c| c == '@' || c == '\\' || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let authority = authority.strip_suffix(default_port).unwrap_or(authority);
    Some(format!("{scheme}://{authority}"))
}

/// Axum middleware: reject unsafe requests from disallowed origins with 403.
pub async fn csrf_protect(
    State(guard): State<CsrfGuard>,
    request: Request,
    next: Next,
) -> Response {
    match guard.check(request.method(), request.headers()) {
        Ok(()) => next.run(request).await,
        Err(reason) => {
            tracing::warn!(
                method = %request.method(),
                path = %request.uri().path(),
                origin = ?request.headers().get(header::ORIGIN),
                referer = ?request.headers().get(header::REFERER),
                reason,
                "rejected cross-origin write request"
            );
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "cross-origin request rejected"})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    fn guard() -> CsrfGuard {
        CsrfGuard::new(&["https://rileyleff.com".to_string()]).unwrap()
    }

    fn app() -> Router {
        Router::new()
            .route("/thing", get(|| async { "ok" }).post(|| async { "ok" }))
            .route(
                "/thing/{id}",
                axum::routing::patch(|| async { "ok" }).delete(|| async { "ok" }),
            )
            .layer(axum::middleware::from_fn_with_state(guard(), csrf_protect))
    }

    async fn status(method: &str, uri: &str, headers: &[(&str, &str)]) -> StatusCode {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        app()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[test]
    fn normalize() {
        assert_eq!(
            normalize_origin("https://rileyleff.com").as_deref(),
            Some("https://rileyleff.com")
        );
        assert_eq!(
            normalize_origin("HTTPS://RileyLeff.com:443/blog/x?y#z").as_deref(),
            Some("https://rileyleff.com")
        );
        assert_eq!(
            normalize_origin("http://localhost:5173/").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(normalize_origin("null"), None);
        assert_eq!(normalize_origin(""), None);
        assert_eq!(normalize_origin("javascript://rileyleff.com"), None);
        assert_eq!(normalize_origin("https://"), None);
        assert_eq!(normalize_origin("https://rileyleff.com@evil.com/"), None);
    }

    #[test]
    fn rejects_invalid_config() {
        assert!(CsrfGuard::new(&["rileyleff.com".to_string()]).is_err());
        assert!(CsrfGuard::new(&["*".to_string()]).is_err());
    }

    #[tokio::test]
    async fn allows_same_origin_writes() {
        let o = [("origin", "https://rileyleff.com")];
        assert_eq!(status("POST", "/thing", &o).await, StatusCode::OK);
        assert_eq!(status("PATCH", "/thing/1", &o).await, StatusCode::OK);
        assert_eq!(status("DELETE", "/thing/1", &o).await, StatusCode::OK);
        assert_eq!(
            status("POST", "/thing", &[("origin", "https://rileyleff.com:443")]).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn rejects_sibling_subdomain_and_foreign_origins() {
        for origin in [
            "https://forestroyale.rileyleff.com",
            "https://evil.com",
            "https://rileyleff.com.evil.com",
            "http://rileyleff.com",
            "https://rileyleff.com:8443",
            "null",
        ] {
            let h = [("origin", origin), ("cookie", "auth_access=x")];
            assert_eq!(
                status("POST", "/thing", &h).await,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
            assert_eq!(
                status("PATCH", "/thing/1", &h).await,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
            assert_eq!(
                status("DELETE", "/thing/1", &h).await,
                StatusCode::FORBIDDEN,
                "{origin}"
            );
        }
    }

    #[tokio::test]
    async fn bad_origin_not_rescued_by_referer_or_bearer() {
        let h = [
            ("origin", "https://forestroyale.rileyleff.com"),
            ("referer", "https://rileyleff.com/blog/post"),
            ("authorization", "Bearer abc"),
        ];
        assert_eq!(status("POST", "/thing", &h).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn referer_fallback() {
        assert_eq!(
            status(
                "POST",
                "/thing",
                &[("referer", "https://rileyleff.com/blog/post")]
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            status(
                "POST",
                "/thing",
                &[("referer", "https://forestroyale.rileyleff.com/play")]
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn missing_origin_requires_bearer() {
        assert_eq!(
            status("POST", "/thing", &[("cookie", "auth_access=x")]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(status("POST", "/thing", &[]).await, StatusCode::FORBIDDEN);
        // Non-Bearer Authorization falls back to cookie auth, so it must not bypass.
        assert_eq!(
            status("POST", "/thing", &[("authorization", "Basic abc")]).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status("POST", "/thing", &[("authorization", "Bearer abc")]).await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn safe_methods_are_not_checked() {
        let h = [("origin", "https://evil.com")];
        assert_eq!(status("GET", "/thing", &h).await, StatusCode::OK);
        assert_eq!(status("GET", "/thing", &[]).await, StatusCode::OK);
    }
}
