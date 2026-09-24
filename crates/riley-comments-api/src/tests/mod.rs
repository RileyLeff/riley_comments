//! Router-level tests.
//!
//! Tests that need Postgres read `TEST_DATABASE_URL` (e.g.
//! `postgres://postgres:test@127.0.0.1:55433/comments_test`). Each one runs in
//! its own freshly migrated schema, and is skipped with a notice when the
//! variable is unset.

mod admin;
mod support;
