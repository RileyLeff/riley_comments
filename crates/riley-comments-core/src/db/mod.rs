pub mod comments;
pub mod custom_emoji;
pub mod reactions;

use crate::config::DatabaseConfig;
use crate::Result;
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn connect(config: &DatabaseConfig) -> Result<PgPool> {
    let url = config.url.resolve()?;
    let mut opts = PgPoolOptions::new().max_connections(config.max_connections);

    if let Some(schema) = &config.schema {
        if !schema.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(crate::Error::Config(format!(
                "invalid schema name: {schema}"
            )));
        }
        let schema = schema.clone();
        opts = opts.after_connect(move |conn, _meta| {
            let schema = schema.clone();
            Box::pin(async move {
                sqlx::Executor::execute(
                    &mut *conn,
                    format!("SET search_path TO \"{schema}\"").as_str(),
                )
                .await?;
                Ok(())
            })
        });
    }

    let pool = opts.connect(&url).await?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
