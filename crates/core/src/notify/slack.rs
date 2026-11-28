//! Slack webhook notification sender.
//!
//! Sends messages to a Slack channel via an incoming webhook URL.

use tracing::{debug, info, warn};

use crate::errors::NotificationError;

/// Slack incoming-webhook notifier.
pub struct SlackNotifier {
    webhook_url: String,
    http: reqwest::Client,
}

impl SlackNotifier {
    /// Create a new Slack notifier targeting the given webhook URL.
    pub fn new(webhook_url: String) -> Self {
        info!("initializing Slack notifier");
        Self {
            webhook_url,
            http: reqwest::Client::new(),
        }
    }

    /// Send a message to the configured Slack channel.
    ///
    /// The `message` is sent as a simple `text` payload. Slack Markdown
    /// formatting (mrkdwn) is supported.
    pub async fn send_message(&self, message: &str) -> Result<(), NotificationError> {
        debug!(len = message.len(), "sending Slack message");

        let payload = serde_json::json!({
            "text": message,
            "unfurl_links": false,
            "unfurl_media": false,
        });

        let resp = self
            .http
            .post(&self.webhook_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| NotificationError::HttpError(e.without_url()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "Slack webhook returned error");
            return Err(NotificationError::SlackError(format!(
                "HTTP {}: {}",
                status, body
            )));
        }

        info!("Slack message sent successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slack_notifier_construction() {
        let notifier = SlackNotifier::new("https://hooks.slack.com/test".into());
        assert_eq!(notifier.webhook_url, "https://hooks.slack.com/test");
    }
}
