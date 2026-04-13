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
        let total_commits = svn_to_git + git_to_svn;

        // Build direction summary
        let mut direction_parts = vec![];
        if svn_to_git > 0 {
            direction_parts.push(format!(
                "**SVN → Git**: {} {}",
                svn_to_git,
                if svn_to_git == 1 { "commit" } else { "commits" }
            ));
        }
        if git_to_svn > 0 {
            direction_parts.push(format!(
                "**Git → SVN**: {} {}",
                git_to_svn,
                if git_to_svn == 1 { "commit" } else { "commits" }
            ));
        }
        let direction_summary = direction_parts.join("  ·  ");

        // Build card body
        let mut body = vec![
            // Header
            json!({
                "type": "TextBlock",
                "text": format!(
                    "✅ Sync completed — {}",
                    repo_name
                ),
                "weight": "Bolder",
                "size": "Medium",
                "color": "Good",
                "wrap": true
            }),
            // Direction summary
            json!({
                "type": "TextBlock",
                "text": direction_summary,
                "wrap": true,
                "spacing": "Small"
            }),
        ];

        if conflicts > 0 {
            body.push(json!({
                "type": "TextBlock",
                "text": format!("⚠️ {} {} detected", conflicts, if conflicts == 1 { "conflict" } else { "conflicts" }),
                "color": "Warning",
                "wrap": true,
                "spacing": "Small"
            }));
        }

        // Rich commit details from the "commits" array
        let commits = event.get("commits").and_then(|v| v.as_array());
        if let Some(commits) = commits {
            if !commits.is_empty() {
                // Separator before commits
                body.push(json!({
                    "type": "TextBlock",
                    "text": format!(
                        "**{}:**",
                        if total_commits == 1 { "Commit" } else { "Commits" }
                    ),
                    "spacing": "Medium",
                    "separator": true
                }));

                for commit in commits.iter().take(10) {
                    let author = commit.get("author").and_then(|v| v.as_str()).unwrap_or("Unknown");
                    let message = commit.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let rev_id = commit.get("revision_id").and_then(|v| v.as_str()).unwrap_or("");
                    let files_changed = commit.get("files_changed").and_then(|v| v.as_i64()).unwrap_or(0);
                    let direction = commit.get("direction").and_then(|v| v.as_str()).unwrap_or("");

                    let direction_icon = if direction == "svn_to_git" { "→Git" } else { "→SVN" };

                    // First line as summary, rest as details
                    let first_line = message.lines().next().unwrap_or("(no message)");
                    let summary = if first_line.len() > 120 {
                        format!("{}…", &first_line[..120])
                    } else {
                        first_line.to_string()
                    };

                    // Commit entry with author, rev, direction, and file count
                    body.push(json!({
                        "type": "Container",
                        "spacing": "Small",
                        "items": [
                            {
                                "type": "TextBlock",
                                "text": summary,
                                "wrap": true,
                                "weight": "Bolder",
                                "size": "Small"
                            },
                            {
                                "type": "TextBlock",
                                "text": format!(
                                    "{} by **{}**  ·  `{}`  ·  {} {} changed",
                                    direction_icon,
                                    author,
                                    rev_id,
                                    files_changed,
                                    if files_changed == 1 { "file" } else { "files" }
                                ),
                                "wrap": true,
                                "size": "Small",
                                "isSubtle": true,
                                "spacing": "None"
                            }
                        ]
                    }));
                }
            }
        } else {
            // Fallback: use legacy "messages" field if "commits" not available
            let messages: Vec<String> = event
                .get("messages")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| {
                            let truncated = if s.len() > 100 {
                                format!("{}…", &s[..100])
                            } else {
                                s.to_string()
                            };
                            format!("• {}", truncated)
                        })
                        .collect()
                })
                .unwrap_or_default();

            if !messages.is_empty() {
                body.push(json!({
                    "type": "TextBlock",
                    "text": messages.join("\n"),
                    "wrap": true,
                    "size": "Small",
                    "spacing": "Medium",
                    "separator": true
                }));
            }
        }

        Some(serde_json::Value::Array(body))
    }

    fn format_sync_failed(&self, event: &serde_json::Value) -> serde_json::Value {
        let repo_name = event.get("repo_name").and_then(|v| v.as_str()).unwrap_or("Unknown");
        let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("Unknown error");
        let is_permanent = event.get("is_permanent").and_then(|v| v.as_bool()).unwrap_or(false);

        let (icon, title) = if is_permanent {
            ("🛑", "Sync failed — permanent error")
        } else {
            ("❌", "Sync failed")
        };

        // Truncate error message for card display
        let short_error = if error.len() > 300 {
            format!("{}…", &error[..300])
        } else {
            error.to_string()
        };

        json!([
            {
                "type": "TextBlock",
                "text": format!("{} {} — {}", icon, title, repo_name),
                "weight": "Bolder",
                "size": "Medium",
                "color": "Attention",
                "wrap": true
            },
            {
                "type": "TextBlock",
                "text": short_error,
                "wrap": true,
                "size": "Small",
                "color": "Attention",
                "spacing": "Small"
            }
        ])
    }

    fn format_import_progress(&self, event: &serde_json::Value) -> Option<serde_json::Value> {
        let phase = event.get("phase").and_then(|v| v.as_str()).unwrap_or("");
        let repo_name = event.get("repo_name").and_then(|v| v.as_str()).unwrap_or("Repository");

        match phase {
            "completed" => {
                let total_revs = event.get("total_revs").and_then(|v| v.as_i64()).unwrap_or(0);
                let commits = event.get("commits_created").and_then(|v| v.as_i64()).unwrap_or(0);

                Some(json!([
                    {
                        "type": "TextBlock",
                        "text": format!("📦 Import completed — {}", repo_name),
                        "weight": "Bolder",
                        "size": "Medium",
                        "color": "Good",
                        "wrap": true
                    },
                    {
                        "type": "FactSet",
                        "facts": [
                            {"title": "SVN Revisions Processed", "value": format!("{}", total_revs)},
                            {"title": "Git Commits Created", "value": format!("{}", commits)},
                        ]
                    }
                ]))
            }
            "failed" => {
                let error = event.get("error").and_then(|v| v.as_str()).unwrap_or("");
                let short_error = if error.len() > 300 {
                    format!("{}…", &error[..300])
                } else {
                    error.to_string()
                };

                let mut body = vec![json!({
                    "type": "TextBlock",
                    "text": format!("❌ Import failed — {}", repo_name),
                    "weight": "Bolder",
                    "size": "Medium",
                    "color": "Attention",
                    "wrap": true
                })];

                if !short_error.is_empty() {
                    body.push(json!({
                        "type": "TextBlock",
                        "text": short_error,
                        "wrap": true,
                        "size": "Small",
                        "color": "Attention"
                    }));
                }

                Some(serde_json::Value::Array(body))
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
            "color": "Good",
            "wrap": true
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
            "color": "Warning",
            "wrap": true
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

    let violation_text = if violations.len() > 5 {
        let shown: Vec<&str> = violations.iter().take(5).map(|s| s.as_str()).collect();
        format!("{}  \nand {} more", shown.join("  \n"), violations.len() - 5)
    } else {
        violations.join("  \n")
    };

    json!([
        {
            "type": "TextBlock",
            "text": format!("{} {} — {}", icon, title, repo_name),
            "weight": "Bolder",
            "size": "Medium",
            "color": "Warning",
            "wrap": true
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
            "color": "Attention",
            "wrap": true
        },
        {
            "type": "TextBlock",
            "text": format!(
                "Sync has been paused after {} consecutive permanent errors. Manual intervention is required — use **Skip Commit** or **Retry** from the RepoSync dashboard to resume.",
                consecutive_errors
            ),
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
        let notifier = TeamsNotifier::new("https://test.webhook.office.com/xxx".into());
        let event = json!({
            "type": "repo_sync_completed",
            "repo_name": "Test",
            "svn_to_git": 0,
            "git_to_svn": 0,
        });
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
    fn test_format_sync_with_rich_commits() {
        let notifier = TeamsNotifier::new("https://test".into());
        let event = json!({
            "type": "repo_sync_completed",
            "repo_name": "EDM Server Load Simulator / dev/RepoSyncValidation",
            "svn_to_git": 0,
            "git_to_svn": 1,
            "conflicts": 0,
            "commits": [
                {
                    "direction": "git_to_svn",
                    "author": "Chris Case",
                    "message": "Updated configuration for load test parameters\n\nAdjusted thread count and timeout values.",
                    "files_changed": 3,
                    "revision_id": "abc1234"
                }
            ]
        });
        let card = notifier.format_sync_completed(&event).unwrap();
        let card_str = card.to_string();
        assert!(card_str.contains("Chris Case"));
        assert!(card_str.contains("abc1234"));
        assert!(card_str.contains("3 files changed"));
        assert!(card_str.contains("Updated configuration for load test parameters"));
        // Should not include body lines in summary
        assert!(!card_str.contains("Adjusted thread count"));
    }

    #[test]
    fn test_format_sync_fallback_messages() {
        let notifier = TeamsNotifier::new("https://test".into());
        let event = json!({
            "type": "repo_sync_completed",
            "repo_name": "Test Repo",
            "svn_to_git": 1,
            "git_to_svn": 0,
            "messages": ["Fix bug in parser"],
        });
        let card = notifier.format_sync_completed(&event).unwrap();
        let card_str = card.to_string();
        assert!(card_str.contains("Fix bug in parser"));
    }

    #[test]
    fn test_format_circuit_breaker() {
        let card = format_circuit_breaker("EDM Repo", 3);
        let text = card[0]["text"].as_str().unwrap();
        assert!(text.contains("Circuit breaker"));
        assert!(text.contains("EDM Repo"));
    }

    #[test]
    fn test_format_path_violation_many_files() {
        let violations: Vec<String> = (0..8).map(|i| format!("bad/file{}.txt", i)).collect();
        let card = format_path_violation("Test Repo", &violations, false);
        let card_str = card.to_string();
        assert!(card_str.contains("and 3 more"));
    }
}
