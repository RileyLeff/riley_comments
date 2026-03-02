pub mod auth;
pub mod error;
pub mod routes;

use auth::JwksCache;
use axum::http::Method;
use axum::Router;
use riley_comments_core::config::Config;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub config: Config,
    pub pool: PgPool,
    pub jwks: Arc<JwksCache>,
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

    let cors = build_cors(&config.server.cors_origins);

    let state = Arc::new(AppState {
        config,
        pool,
        jwks: Arc::clone(&jwks),
    });

    let app = Router::new()
        .merge(routes::router(Arc::clone(&state)))
        .layer(axum::Extension(jwks))
        .layer(cors)
        .layer(TraceLayer::new_for_http());

    tracing::info!(%addr, "starting server");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service()).await?;

    Ok(())
}

fn build_cors(origins: &[String]) -> CorsLayer {
    if origins.is_empty() {
        CorsLayer::new()
    } else if origins.len() == 1 && origins[0] == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
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
