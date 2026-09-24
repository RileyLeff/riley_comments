use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::{get, post};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header};
use riley_comments_core::config::{
    AuthConfig, CommentsConfig, Config, ConfigValue, DatabaseConfig, ServerConfig,
};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

use crate::AppState;
use crate::auth::{Claims, JwksCache, RoleChecker};
use crate::notifications::NotificationsClient;

const SECRET: &[u8] = b"riley-comments-test-secret";
const ISSUER: &str = "rileyleff";

/// A riley-auth URL nothing listens on, for "auth is down" cases.
pub const DEAD_ME_URL: &str = "http://127.0.0.1:1/auth/me";

/// Mint an access token the way riley-auth would.
pub fn token(sub: Uuid, username: &str, role: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: sub.to_string(),
        username: username.to_string(),
        role: role.to_string(),
        aud: Some(ISSUER.to_string()),
        iss: Some(ISSUER.to_string()),
        iat: Some(now),
        exp: now + 3600,
    };
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(SECRET),
    )
    .unwrap()
}

/// Serve `router` on an ephemeral local port and return its base URL.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

/// A stand-in for riley-auth's `GET /auth/me`, answering from a table of
/// access token -> (user id, current role).
#[derive(Clone, Default)]
pub struct MockAuth {
    users: Arc<Mutex<HashMap<String, (Uuid, String)>>>,
    hits: Arc<Mutex<usize>>,
    pub me_url: String,
}

impl MockAuth {
    pub async fn start() -> Self {
        let mut mock = Self::default();
        let state = mock.clone();
        let router = Router::new().route(
            "/auth/me",
            get(move |req: Request| {
                let state = state.clone();
                async move {
                    *state.hits.lock().unwrap() += 1;
                    let token = req
                        .headers()
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|c| c.strip_prefix("auth_access="))
                        .map(str::to_string);
                    let user = token.and_then(|t| state.users.lock().unwrap().get(&t).cloned());
                    match user {
                        Some((id, role)) => {
                            Json(serde_json::json!({"id": id, "username": "u", "role": role}))
                                .into_response()
                        }
                        None => StatusCode::UNAUTHORIZED.into_response(),
                    }
                }
            }),
        );
        mock.me_url = format!("{}/auth/me", serve(router).await);
        mock
    }

    /// Register what riley-auth currently says about the holder of `token`.
    pub fn set(&self, token: &str, id: Uuid, role: &str) {
        self.users
            .lock()
            .unwrap()
            .insert(token.to_string(), (id, role.to_string()));
    }

    pub fn hits(&self) -> usize {
        *self.hits.lock().unwrap()
    }
}

/// A stand-in for riley-notifications that records every payload.
#[derive(Clone, Default)]
pub struct MockNotifications {
    received: Arc<Mutex<Vec<serde_json::Value>>>,
    base: String,
}

impl MockNotifications {
    pub async fn start() -> Self {
        let mut mock = Self::default();
        let received = Arc::clone(&mock.received);
        let router = Router::new().route(
            "/notifications",
            post(move |Json(v): Json<serde_json::Value>| {
                let received = Arc::clone(&received);
                async move {
                    received.lock().unwrap().push(v);
                    StatusCode::CREATED
                }
            }),
        );
        mock.base = serve(router).await;
        mock
    }

    pub fn client(&self) -> NotificationsClient {
        NotificationsClient::new(self.base.clone(), "/notifications", "test".to_string())
    }

    pub fn received(&self) -> Vec<serde_json::Value> {
        self.received.lock().unwrap().clone()
    }

    /// Wait until at least `n` notifications arrived, then a little longer so
    /// stray extra sends would show up too.
    pub async fn settle(&self, n: usize) -> Vec<serde_json::Value> {
        for _ in 0..200 {
            if self.received.lock().unwrap().len() >= n {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.received()
    }
}

/// A fresh, migrated schema in the database at `TEST_DATABASE_URL`.
pub struct TestDb {
    pub pool: PgPool,
    admin: PgPool,
    schema: String,
}

impl TestDb {
    /// `None` (with a notice) when `TEST_DATABASE_URL` is unset.
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL not set; skipping database test");
            return None;
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect to TEST_DATABASE_URL");
        let schema = format!("test_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin)
            .await
            .unwrap();
        let pool = riley_comments_core::db::connect(&DatabaseConfig {
            url: ConfigValue::Direct(url),
            max_connections: 5,
            schema: Some(schema.clone()),
        })
        .await
        .unwrap();
        riley_comments_core::db::migrate(&pool).await.unwrap();
        Some(Self {
            pool,
            admin,
            schema,
        })
    }

    pub async fn cleanup(self) {
        self.pool.close().await;
        let _ = sqlx::query(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
            .execute(&self.admin)
            .await;
    }
}

/// A pool that never connects, for tests that must not reach the database.
pub fn no_db() -> PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap()
}

pub fn config() -> Config {
    Config {
        server: ServerConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            cors_origins: vec![],
            behind_proxy: false,
            csrf_origins: Some(vec!["https://rileyleff.com".to_string()]),
        },
        database: DatabaseConfig {
            url: ConfigValue::Direct(String::new()),
            max_connections: 1,
            schema: None,
        },
        auth: AuthConfig {
            jwks_url: "http://unused/.well-known/jwks.json".to_string(),
            expected_issuer: Some(ISSUER.to_string()),
            expected_audience: Some(ISSUER.to_string()),
        },
        comments: CommentsConfig {
            max_depth: 3,
            max_body_length: 10_000,
        },
        r2: None,
        notifications: None,
    }
}

pub fn app(pool: PgPool, me_url: &str, notif: Option<NotificationsClient>) -> Router {
    let jwks = JwksCache::with_keys(
        vec![(DecodingKey::from_secret(SECRET), Algorithm::HS256)],
        Some(ISSUER.to_string()),
        Some(ISSUER.to_string()),
    );
    let state = Arc::new(AppState {
        config: config(),
        pool,
        jwks: Arc::new(jwks),
        roles: RoleChecker::new(me_url.to_string()),
        r2: None,
        notif,
    });
    crate::app(state).unwrap()
}

/// Send a request (Bearer-authenticated when `token` is given) and return the
/// status and JSON body (`Null` when the body is empty or not JSON).
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    send(app, req).await
}

pub async fn send(app: &Router, req: Request) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Post a comment as `token` and return its id.
pub async fn post_comment(
    app: &Router,
    token: &str,
    entity: &str,
    parent: Option<Uuid>,
    body: &str,
) -> Uuid {
    let (status, json) = call(
        app,
        "POST",
        "/comments",
        Some(token),
        Some(serde_json::json!({
            "entity_type": "blog",
            "entity_id": entity,
            "parent_id": parent,
            "body": body,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{json}");
    json["id"].as_str().unwrap().parse().unwrap()
}
