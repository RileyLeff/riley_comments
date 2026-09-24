//! Admin actions must be confirmed live against riley-auth, not trusted from
//! the (long-lived) JWT role claim.

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use uuid::Uuid;

use super::support::*;

async fn upload_emoji(app: &axum::Router, token: &str) -> StatusCode {
    let body = "--X\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nparty\r\n--X--\r\n";
    let req = Request::builder()
        .method("POST")
        .uri("/emoji")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "multipart/form-data; boundary=X")
        .body(Body::from(body))
        .unwrap();
    send(app, req).await.0
}

#[tokio::test]
async fn emoji_management_rechecks_admin_role_live() {
    let auth = MockAuth::start().await;
    let app = app(no_db(), &auth.me_url, None);

    // JWT still says admin, but riley-auth says the user was demoted.
    let demoted_id = Uuid::new_v4();
    let demoted = token(demoted_id, "demoted", "admin");
    auth.set(&demoted, demoted_id, "user");
    let (status, _) = call(&app, "DELETE", "/emoji/party", Some(&demoted), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(upload_emoji(&app, &demoted).await, StatusCode::FORBIDDEN);

    // A token riley-auth no longer accepts (revoked / user deleted).
    let unknown = token(Uuid::new_v4(), "ghost", "admin");
    let (status, _) = call(&app, "DELETE", "/emoji/party", Some(&unknown), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // riley-auth answering for a different user is not a confirmation.
    let mismatched = token(Uuid::new_v4(), "mismatch", "admin");
    auth.set(&mismatched, Uuid::new_v4(), "admin");
    let (status, _) = call(&app, "DELETE", "/emoji/party", Some(&mismatched), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A confirmed admin gets past the gate (and then fails only because this
    // test app has no R2 storage configured).
    let admin_id = Uuid::new_v4();
    let admin = token(admin_id, "riley", "admin");
    auth.set(&admin, admin_id, "admin");
    let (status, json) = call(&app, "DELETE", "/emoji/party", Some(&admin), None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{json}");
    assert_eq!(
        upload_emoji(&app, &admin).await,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn non_admin_claims_are_rejected_without_calling_riley_auth() {
    let auth = MockAuth::start().await;
    let app = app(no_db(), &auth.me_url, None);

    let id = Uuid::new_v4();
    let user = token(id, "someone", "user");
    // Even if riley-auth would now say admin, a user-role token needs a refresh first.
    auth.set(&user, id, "admin");
    let (status, _) = call(&app, "DELETE", "/emoji/party", Some(&user), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(upload_emoji(&app, &user).await, StatusCode::FORBIDDEN);
    assert_eq!(auth.hits(), 0);
}

#[tokio::test]
async fn admin_actions_fail_closed_when_riley_auth_is_unreachable() {
    let app = app(no_db(), DEAD_ME_URL, None);
    let admin = token(Uuid::new_v4(), "riley", "admin");
    let (status, _) = call(&app, "DELETE", "/emoji/party", Some(&admin), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        upload_emoji(&app, &admin).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn deleting_others_comments_rechecks_admin_role_live() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let auth = MockAuth::start().await;
    let app = app(db.pool.clone(), &auth.me_url, None);

    let author = token(Uuid::new_v4(), "author", "user");
    let target = post_comment(&app, &author, "post", None, "hello").await;
    let delete_uri = format!("/comments/{target}/delete");

    // Demoted admin: JWT says admin, riley-auth says user.
    let demoted_id = Uuid::new_v4();
    let demoted = token(demoted_id, "demoted", "admin");
    auth.set(&demoted, demoted_id, "user");
    let (status, _) = call(&app, "POST", &delete_uri, Some(&demoted), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, json) = call(&app, "GET", &format!("/comments/{target}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["body"], "hello");

    // Plain users still get the ordinary ownership error, without a lookup.
    let hits = auth.hits();
    let other = token(Uuid::new_v4(), "other", "user");
    let (status, _) = call(&app, "POST", &delete_uri, Some(&other), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(auth.hits(), hits);

    // A confirmed admin can moderate.
    let admin_id = Uuid::new_v4();
    let admin = token(admin_id, "riley", "admin");
    auth.set(&admin, admin_id, "admin");
    let (status, _) = call(&app, "POST", &delete_uri, Some(&admin), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    db.cleanup().await;
}

#[tokio::test]
async fn deleting_own_comment_stays_on_the_jwt_path() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    // riley-auth is unreachable: own-comment deletes must not depend on it,
    // even for an admin, while moderating others fails closed.
    let app = app(db.pool.clone(), DEAD_ME_URL, None);

    let admin = token(Uuid::new_v4(), "riley", "admin");
    let own = post_comment(&app, &admin, "post", None, "mine").await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/comments/{own}/delete"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let author = token(Uuid::new_v4(), "author", "user");
    let theirs = post_comment(&app, &author, "post", None, "theirs").await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/comments/{theirs}/delete"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    // Writing comments never touches riley-auth either.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/comments/{theirs}/delete"),
        Some(&author),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    db.cleanup().await;
}
