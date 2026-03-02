use serde::Serialize;
use uuid::Uuid;

/// Fire-and-forget HTTP client for sending notifications to riley_notifications.
#[derive(Clone)]
pub struct NotificationsClient {
    http: reqwest::Client,
    base_url: String,
    api_token: String,
}

#[derive(Serialize)]
struct CreateNotification {
    user_id: Uuid,
    type_name: String,
    title: String,
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<serde_json::Value>,
}

impl NotificationsClient {
    pub fn new(base_url: String, api_token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_token,
        }
    }

    /// Send a notification in the background. Logs errors but never fails the caller.
    pub fn send(
        &self,
        recipient_id: Uuid,
        type_name: &str,
        title: &str,
        body: &str,
        url: Option<&str>,
        metadata: Option<serde_json::Value>,
    ) {
        let payload = CreateNotification {
            user_id: recipient_id,
            type_name: type_name.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            url: url.map(|s| s.to_string()),
            metadata,
        };

        let endpoint = format!("{}/notifications", self.base_url);
        let req = self
            .http
            .post(&endpoint)
            .header("x-api-token", &self.api_token)
            .json(&payload);

        tokio::spawn(async move {
            if let Err(e) = req.send().await {
                tracing::warn!("failed to send notification: {e}");
            }
        });
    }
}

/// Truncate a string to at most `max_len` chars, appending "..." if truncated.
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}
