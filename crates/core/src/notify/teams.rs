//! Microsoft Teams webhook notification sender.
//!
//! Sends Adaptive Card messages to a Teams channel via a Workflows
//! webhook URL. Processes WebSocket broadcast events and formats them
//! as rich cards with color-coded status indicators.

use serde_json::json;
use tracing::{debug, info, warn};

use crate::errors::NotificationError;

/// Teams incoming-webhook notifier via Workflows.
pub struct TeamsNotifier {
    webhook_url: String,
    http: reqwest::Client,
}

impl TeamsNotifier {
    /// Create a new Teams notifier targeting the given webhook URL.
    pub fn new(webhook_url: String) -> Self {
        info!("initializing Teams notifier");
        Self {
            webhook_url,
            http: reqwest::Client::new(),
        }
    }

    /// Send an Adaptive Card to the configured Teams channel.
    pub async fn send_card(&self, card_body: serde_json::Value) -> Result<(), NotificationError> {
        let payload = json!({
            "type": "message",
            "attachments": [{
                "contentType": "application/vnd.microsoft.card.adaptive",
                "content": {
                    "type": "AdaptiveCard",
                    "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                    "version": "1.4",
                    "body": card_body,
                }
            }]
        });

        debug!("sending Teams Adaptive Card");

        let resp = self
            .http
            .post(&self.webhook_url)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(NotificationError::HttpError)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "Teams webhook returned error");
            return Err(NotificationError::SlackError(format!(
                "Teams HTTP {}: {}",
                status, body
            )));
        }

        debug!("Teams card sent successfully");
        Ok(())
    }

    /// Process a WebSocket broadcast event and send to Teams if relevant.
    ///
    /// Returns `true` if a notification was sent, `false` if the event
    /// was filtered out (e.g., no-change sync cycle).
    pub async fn process_event(&self, event_json: &str) -> bool {
        let event: serde_json::Value = match serde_json::from_str(event_json) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

        let card_body = match event_type {
            "repo_sync_completed" => self.format_sync_completed(&event),
            "repo_sync_failed" => Some(self.format_sync_failed(&event)),
            "repo_import_progress" => self.format_import_progress(&event),
            _ => None,
        };

        if let Some(body) = card_body {
            if let Err(e) = self.send_card(body).await {
                warn!(error = %e, event_type, "failed to send Teams notification");
            }
            true
        } else {
            false
        }
    }

    /// Send a test notification card.
    pub async fn send_test(&self) -> Result<(), NotificationError> {
        let body = json!([
            {
                "type": "TextBlock",
                "text": "RepoSync — Test Notification",
                "weight": "Bolder",
                "size": "Medium",
                "color": "Good"
            },
            {
                "type": "TextBlock",
                "text": "Teams webhook is configured correctly. You will receive notifications for sync activity, errors, and other events.",
                "wrap": true
            }
        ]);
        self.send_card(body).await
    }

    // --- Event formatters ---

    fn format_sync_completed(&self, event: &serde_json::Value) -> Option<serde_json::Value> {
        let svn_to_git = event.get("svn_to_git").and_then(|v| v.as_i64()).unwrap_or(0);
        let git_to_svn = event.get("git_to_svn").and_then(|v| v.as_i64()).unwrap_or(0);

        // Skip no-change cycles to avoid flooding
        if svn_to_git == 0 && git_to_svn == 0 {
            return None;
        }

        let repo_name = event.get("repo_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let conflicts = event.get("conflicts").and_then(|v| v.as_i64()).unwrap_or(0);

        let mut facts = vec![];
        if svn_to_git > 0 {
            facts.push(json!({"title": "SVN → Git", "value": format!("{} commit(s)", svn_to_git)}));
        }
        if git_to_svn > 0 {
            facts.push(json!({"title": "Git → SVN", "value": format!("{} commit(s)", git_to_svn)}));
        }
        if conflicts > 0 {
            facts.push(json!({"title": "Conflicts", "value": format!("{}", conflicts)}));
        }

        // Include commit messages if available
        let messages: Vec<String> = event
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| {
                        let truncated = if s.len() > 80 { format!("{}...", &s[..80]) } else { s.to_string() };
                        format!("• {}", truncated)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut body = vec![
            json!({
                "type": "TextBlock",
                "text": format!("✅ Sync completed — {}", repo_name),
                "weight": "Bolder",
                "size": "Medium",
                "color": "Good"
            }),
            json!({
                "type": "FactSet",
                "facts": facts
            }),
        ];

        if !messages.is_empty() {
            body.push(json!({
                "type": "TextBlock",
                "text": messages.join("\n"),
                "wrap": true,
                "size": "Small",
                "color": "Default"
            }));
        }

        Some(serde_json::Value::Array(body))
    }

    fn format_sync_failed(&self, event: &serde_json::Value) -> serde_json::Value {
        let repo_name = event.get("repo_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("Unknown error");
        let is_permanent = event.get("is_permanent").and_then(|v| v.as_bool()).unwrap_or(false);

        let (icon, title) = if is_permanent {
            ("🛑", "Sync failed (permanent error)")
        } else {
            ("❌", "Sync failed")
        };

        // Truncate error message for card display
        let short_error = if error.len() > 200 {
            format!("{}...", &error[..200])
        } else {
            error.to_string()
        };

        json!([
            {
                "type": "TextBlock",
                "text": format!("{} {} — {}", icon, title, repo_name),
                "weight": "Bolder",
                "size": "Medium",
                "color": "Attention"
            },
            {
                "type": "TextBlock",
                "text": short_error,
                "wrap": true,
                "size": "Small",
                "color": "Attention"
            }
        ])
    }

    fn format_import_progress(&self, event: &serde_json::Value) -> Option<serde_json::Value> {
        let phase = event.get("phase").and_then(|v| v.as_str()).unwrap_or("");

        // Only send on completion or failure, not every progress update
        match phase {
            "completed" => {
                let _repo_id = event.get("repo_id").and_then(|v| v.as_str()).unwrap_or("");
                let total_revs = event.get("total_revs").and_then(|v| v.as_i64()).unwrap_or(0);
                let commits = event.get("commits_created").and_then(|v| v.as_i64()).unwrap_or(0);

                Some(json!([
                    {
                        "type": "TextBlock",
                        "text": "📦 Import completed",
                        "weight": "Bolder",
                        "size": "Medium",
                        "color": "Good"
                    },
                    {
                        "type": "FactSet",
                        "facts": [
                            {"title": "Revisions", "value": format!("{}", total_revs)},
                            {"title": "Commits", "value": format!("{}", commits)},
                        ]
                    }
                ]))
            }
            "failed" => {
                Some(json!([
                    {
                        "type": "TextBlock",
                        "text": "📦 Import failed",
                        "weight": "Bolder",
                        "size": "Medium",
                        "color": "Attention"
                    }
                ]))
            }
            _ => None,
        }
    }
}

/// Format a branch pair creation event as a Teams card body.
pub fn format_branch_created(repo_name: &str, git_branch: &str, svn_branch: &str) -> serde_json::Value {
    json!([
        {
            "type": "TextBlock",
            "text": format!("🌿 Branch pair created — {}", repo_name),
            "weight": "Bolder",
            "size": "Medium",
            "color": "Good"
        },
        {
            "type": "FactSet",
            "facts": [
                {"title": "Git Branch", "value": git_branch},
                {"title": "SVN Branch", "value": svn_branch},
            ]
        }
    ])
}

/// Format a branch pair deletion event as a Teams card body.
pub fn format_branch_deleted(repo_name: &str, git_branch: &str) -> serde_json::Value {
    json!([
        {
            "type": "TextBlock",
            "text": format!("🗑️ Branch pair deleted — {}", repo_name),
            "weight": "Bolder",
            "size": "Medium",
            "color": "Warning"
        },
        {
            "type": "FactSet",
            "facts": [
                {"title": "Branch", "value": git_branch},
            ]
        }
    ])
}

/// Format a path violation event as a Teams card body.
pub fn format_path_violation(repo_name: &str, violations: &[String], skipped_all: bool) -> serde_json::Value {
    let (icon, title) = if skipped_all {
        ("⚠️", "Commit skipped — all files violate path rules")
    } else {
        ("⚠️", "Files filtered from commit")
    };

    let violation_text = if violations.len() > 3 {
        format!("{} and {} more", violations[..3].join(", "), violations.len() - 3)
    } else {
        violations.join(", ")
    };

    json!([
        {
            "type": "TextBlock",
            "text": format!("{} {} — {}", icon, title, repo_name),
            "weight": "Bolder",
            "size": "Medium",
            "color": "Warning"
        },
        {
            "type": "TextBlock",
            "text": violation_text,
            "wrap": true,
            "size": "Small"
        }
    ])
}

/// Format a circuit breaker event as a Teams card body.
pub fn format_circuit_breaker(repo_name: &str, consecutive_errors: i64) -> serde_json::Value {
    json!([
        {
            "type": "TextBlock",
            "text": format!("🛑 Circuit breaker triggered — {}", repo_name),
            "weight": "Bolder",
            "size": "Medium",
            "color": "Attention"
        },
        {
            "type": "TextBlock",
            "text": format!("Sync paused after {} consecutive permanent errors. Manual intervention required (Skip Commit or Retry).", consecutive_errors),
            "wrap": true,
            "color": "Attention"
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_teams_notifier_construction() {
        let notifier = TeamsNotifier::new("https://test.webhook.office.com/xxx".into());
        assert_eq!(notifier.webhook_url, "https://test.webhook.office.com/xxx");
    }

    #[test]
    fn test_skip_no_change_sync() {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let notifier = TeamsNotifier::new("https://test.webhook.office.com/xxx".into());
        let event = json!({
            "type": "repo_sync_completed",
            "repo_name": "Test",
            "svn_to_git": 0,
            "git_to_svn": 0,
        });
        // format_sync_completed should return None for no-change cycles
        assert!(notifier.format_sync_completed(&event).is_none());
    }

    #[test]
    fn test_format_sync_with_changes() {
        let notifier = TeamsNotifier::new("https://test".into());
        let event = json!({
            "type": "repo_sync_completed",
            "repo_name": "EDM Repo",
            "svn_to_git": 3,
            "git_to_svn": 1,
            "conflicts": 0,
        });
        let card = notifier.format_sync_completed(&event);
        assert!(card.is_some());
    }

    #[test]
    fn test_format_circuit_breaker() {
        let card = format_circuit_breaker("EDM Repo", 3);
        let text = card[0]["text"].as_str().unwrap();
        assert!(text.contains("Circuit breaker"));
        assert!(text.contains("EDM Repo"));
    }
}
