use axum::http::StatusCode;
use uuid::Uuid;

use super::support::*;

async fn react(app: &axum::Router, token: &str, comment: Uuid, emoji: &str) {
    let (status, json) = call(
        app,
        "POST",
        &format!("/comments/{comment}/reactions"),
        Some(token),
        Some(serde_json::json!({"emoji": emoji})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{json}");
}

#[tokio::test]
async fn repeated_reactions_notify_once() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let notif = MockNotifications::start().await;
    let app = app(db.pool.clone(), DEAD_ME_URL, Some(notif.client()));

    let author_id = Uuid::new_v4();
    let author = token(author_id, "author", "user");
    let fan = token(Uuid::new_v4(), "fan", "user");
    let comment = post_comment(&app, &author, "post", None, "hello").await;

    react(&app, &fan, comment, "👍").await;
    react(&app, &fan, comment, "👍").await;
    react(&app, &fan, comment, "👍").await;
    react(&app, &fan, comment, "🎉").await;
    // Reacting to your own comment never notifies.
    react(&app, &author, comment, "👍").await;

    let sent = notif.settle(2).await;
    let mut emoji: Vec<&str> = sent
        .iter()
        .map(|n| n["metadata"]["emoji"].as_str().unwrap())
        .collect();
    emoji.sort();
    assert_eq!(emoji, vec!["🎉", "👍"], "{sent:?}");
    assert!(sent.iter().all(|n| n["user_id"] == author_id.to_string()));

    // Removing and re-adding is a new reaction, so it notifies again.
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/comments/{comment}/reactions/{}", "%F0%9F%91%8D"),
        Some(&fan),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    react(&app, &fan, comment, "👍").await;
    assert_eq!(notif.settle(3).await.len(), 3);

    db.cleanup().await;
}
