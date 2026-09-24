pub mod auth;
pub mod csrf;
pub mod error;
pub mod notifications;
pub mod routes;

#[cfg(test)]
mod tests;

use auth::JwksCache;
use axum::Router;
use axum::http::Method;
use riley_comments_core::config::Config;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

pub struct R2Client {
    pub client: aws_sdk_s3::Client,
    pub bucket: String,
    pub public_url: String,
}

pub struct AppState {
    pub config: Config,
    pub pool: PgPool,
    pub jwks: Arc<JwksCache>,
    /// Live role check against riley-auth for admin-only actions.
    pub roles: auth::RoleChecker,
    pub r2: Option<R2Client>,
    pub notif: Option<notifications::NotificationsClient>,
}

pub async fn serve(config: Config, pool: PgPool) -> anyhow::Result<()> {
    let addr = SocketAddr::new(config.server.host.parse()?, config.server.port);

    // Set up JWKS cache
    let jwks = Arc::new(JwksCache::new(
        config.auth.jwks_url.clone(),
        config.auth.expected_issuer.clone(),
        config.auth.expected_audience.clone(),
    ));

    // Initial fetch + background refresh
    jwks.refresh().await?;
    jwks.spawn_refresh_task();

    // Admin actions re-check the role against riley-auth (same host as JWKS).
    let roles = auth::RoleChecker::from_jwks_url(&config.auth.jwks_url)?;
    tracing::info!(url = %roles.me_url(), "admin role check endpoint");

    // Set up R2 client if configured
    let r2 = if let Some(r2_config) = &config.r2 {
        let endpoint = r2_config.endpoint.resolve()?;
        let access_key = r2_config.access_key_id.resolve()?;
        let secret_key = r2_config.secret_access_key.resolve()?;

        let creds = aws_credential_types::Credentials::new(
            access_key,
            secret_key,
            None,
            None,
            "riley-comments",
        );

        let s3_config = aws_sdk_s3::Config::builder()
            .endpoint_url(&endpoint)
            .region(aws_sdk_s3::config::Region::new("auto"))
            .credentials_provider(creds)
            .force_path_style(true)
            .build();

        let client = aws_sdk_s3::Client::from_conf(s3_config);
        tracing::info!(bucket = %r2_config.bucket, "R2 client initialized");

        Some(R2Client {
            client,
            bucket: r2_config.bucket.clone(),
            public_url: r2_config.public_url.resolve()?,
        })
    } else {
        tracing::info!("R2 not configured, custom emoji upload disabled");
        None
    };

    let notif = if let Some(notif_config) = &config.notifications {
        let token = notif_config.api_token.resolve()?;
        tracing::info!(url = %notif_config.url, "notifications client initialized");
        Some(notifications::NotificationsClient::new(
            notif_config.url.clone(),
            "/notifications",
            token,
        ))
    } else {
        tracing::info!("notifications not configured, notifications disabled");
        None
    };

    let state = Arc::new(AppState {
        config,
        pool,
        jwks,
        roles,
        r2,
        notif,
    });

    let app = app(state)?;

    tracing::info!(%addr, "starting server");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}

/// Build the full application router (routes + middleware stack).
pub fn app(state: Arc<AppState>) -> anyhow::Result<Router> {
    let cors = build_cors(&state.config.server.cors_origins);

    let csrf_origins = state.config.server.effective_csrf_origins();
    tracing::info!(?csrf_origins, "CSRF allowed origins");
    let csrf = csrf::CsrfGuard::new(&csrf_origins)?;

    // Layer order (outermost first): trace -> CORS -> CSRF -> routes.
    // CORS sits outside CSRF so preflights are answered and 403s carry CORS headers.
    Ok(Router::new()
        .merge(routes::router(Arc::clone(&state)))
        .layer(axum::Extension(Arc::clone(&state.jwks)))
        .layer(axum::middleware::from_fn_with_state(
            csrf,
            csrf::csrf_protect,
        ))
        .layer(cors)
        .layer(TraceLayer::new_for_http()))
}

fn build_cors(origins: &[String]) -> CorsLayer {
    if origins.is_empty() {
        CorsLayer::new()
    } else if origins.len() == 1 && origins[0] == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = origins.iter().filter_map(|o| o.parse().ok()).collect();
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([
                axum::http::header::CONTENT_TYPE,
                axum::http::header::AUTHORIZATION,
            ])
            .allow_credentials(true)
    }
}
