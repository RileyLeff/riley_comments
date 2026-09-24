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

async fn list(app: &axum::Router, query: &str) -> serde_json::Value {
    let (status, json) = call(app, "GET", &format!("/comments/blog/{query}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    json
}

async fn delete(app: &axum::Router, token: &str, id: Uuid) {
    let (status, _) = call(
        app,
        "POST",
        &format!("/comments/{id}/delete"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

fn find(page: &serde_json::Value, id: Uuid) -> Option<serde_json::Value> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id.to_string())
        .cloned()
}

fn assert_placeholder(page: &serde_json::Value, id: Uuid) {
    let c = find(page, id).unwrap_or_else(|| panic!("{id} missing from {page}"));
    assert_eq!(c["deleted"], true);
    assert_eq!(c["body"], "[deleted]");
    assert_eq!(c["username"], "[deleted]");
    assert_eq!(c["user_id"], Uuid::nil().to_string());
    assert_eq!(c["reply_to_username"], serde_json::Value::Null);
    assert_eq!(c["reactions"], serde_json::json!([]));
}

#[tokio::test]
async fn deleted_parents_keep_their_surviving_replies_visible() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let app = app(db.pool.clone(), DEAD_ME_URL, None);
    let alice = token(Uuid::new_v4(), "alice", "user");
    let bob = token(Uuid::new_v4(), "bob", "user");

    // root (deleted) -> mid (deleted) -> leaf (live)
    //                -> gone (deleted, no live replies)
    let root = post_comment(&app, &alice, "thread", None, "root secret").await;
    let mid = post_comment(&app, &bob, "thread", Some(root), "mid secret").await;
    let leaf = post_comment(&app, &alice, "thread", Some(mid), "leaf text").await;
    let gone = post_comment(&app, &bob, "thread", Some(root), "gone secret").await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/comments/{root}/reactions"),
        Some(&bob),
        Some(serde_json::json!({"emoji": "👍"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    delete(&app, &bob, gone).await;
    delete(&app, &bob, mid).await;
    delete(&app, &alice, root).await;

    let page = list(&app, "thread").await;
    assert_placeholder(&page, root);
    assert_placeholder(&page, mid);
    let leaf_item = find(&page, leaf).expect("live reply under deleted parents");
    assert_eq!(leaf_item["body"], "leaf text");
    assert_eq!(leaf_item["deleted"], false);
    assert!(find(&page, gone).is_none(), "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 3);

    let text = page.to_string();
    for secret in ["root secret", "mid secret", "gone secret", "bob"] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }

    // Once the last live reply goes, the whole thread disappears.
    delete(&app, &alice, leaf).await;
    let page = list(&app, "thread").await;
    assert_eq!(page["items"], serde_json::json!([]), "{page}");

    db.cleanup().await;
}

#[tokio::test]
async fn pagination_counts_deleted_roots_with_live_replies() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let app = app(db.pool.clone(), DEAD_ME_URL, None);
    let alice = token(Uuid::new_v4(), "alice", "user");

    let a = post_comment(&app, &alice, "paged", None, "a").await;
    let a_reply = post_comment(&app, &alice, "paged", Some(a), "a reply").await;
    let b = post_comment(&app, &alice, "paged", None, "b").await;
    let b_reply = post_comment(&app, &alice, "paged", Some(b), "b reply").await;
    let c = post_comment(&app, &alice, "paged", None, "c").await;
    delete(&app, &alice, a).await;
    delete(&app, &alice, b_reply).await;
    delete(&app, &alice, b).await;

    // Page 1: the deleted root `a` is kept for its live reply.
    let page = list(&app, "paged?limit=1").await;
    assert_placeholder(&page, a);
    assert!(find(&page, a_reply).is_some(), "{page}");
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["next_cursor"]
        .as_str()
        .expect("more pages")
        .to_string();

    // Page 2: `b` (deleted, nothing live below) is skipped entirely.
    let page = list(
        &app,
        &format!("paged?limit=1&cursor={}", urlencode(&cursor)),
    )
    .await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{page}");
    assert_eq!(items[0]["id"], c.to_string());
    assert!(page.get("next_cursor").is_none(), "{page}");

    db.cleanup().await;
}

fn urlencode(s: &str) -> String {
    s.replace('+', "%2B")
        .replace(':', "%3A")
        .replace(',', "%2C")
}
