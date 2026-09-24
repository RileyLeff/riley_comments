use axum::http::StatusCode;
use uuid::Uuid;

use super::support::*;

#[tokio::test]
async fn deleted_comment_is_not_publicly_readable() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let app = app(db.pool.clone(), DEAD_ME_URL, None);
    let author = token(Uuid::new_v4(), "author", "user");
    let id = post_comment(&app, &author, "post", None, "regrettable text").await;

    let (status, json) = call(&app, "GET", &format!("/comments/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["body"], "regrettable text");

    let (status, _) = call(
        &app,
        "POST",
        &format!("/comments/{id}/delete"),
        Some(&author),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    for who in [None, Some(author.as_str())] {
        let (status, json) = call(&app, "GET", &format!("/comments/{id}"), who, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let text = json.to_string();
        assert!(!text.contains("regrettable"), "{text}");
        assert!(!text.contains("author"), "{text}");
    }

    db.cleanup().await;
}
