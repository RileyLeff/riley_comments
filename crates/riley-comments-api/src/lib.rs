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

pub struct R2Client {
    pub client: aws_sdk_s3::Client,
    pub bucket: String,
    pub public_url: String,
}

pub struct AppState {
    pub config: Config,
    pub pool: PgPool,
    pub jwks: Arc<JwksCache>,
    pub r2: Option<R2Client>,
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

    let cors = build_cors(&config.server.cors_origins);

    let state = Arc::new(AppState {
        config,
        pool,
        jwks: Arc::clone(&jwks),
        r2,
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
