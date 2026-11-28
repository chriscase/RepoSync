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
            .map_err(|e| NotificationError::HttpError(e.without_url()))?;

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
            let repo_name = event
                .get("repo_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            info!(event_type, repo_name, "sending Teams notification");
            if let Err(e) = self.send_card(body).await {
                warn!(error = %e, event_type, repo_name, "failed to send Teams notification");
            } else {
                info!(event_type, repo_name, "Teams notification sent");
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
        let svn_to_git = event
            .get("svn_to_git")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let git_to_svn = event
            .get("git_to_svn")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        // Skip no-change cycles to avoid flooding
        if svn_to_git == 0 && git_to_svn == 0 {
            return None;
        }

        let repo_name = event
            .get("repo_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
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
                    let author = commit
                        .get("author")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let message = commit.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let rev_id = commit
                        .get("revision_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let files_changed = commit
                        .get("files_changed")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let direction = commit
                        .get("direction")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let direction_icon = if direction == "svn_to_git" {
                        "→Git"
                    } else {
                        "→SVN"
                    };

                    // First line as summary, rest as details
                    let first_line = message.lines().next().unwrap_or("(no message)");
                    let summary = truncate_for_card(first_line, 120);

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
                            let truncated = truncate_for_card(s, 100);
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
        let repo_name = event
            .get("repo_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
        let error = event
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        let is_permanent = event
            .get("is_permanent")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let (icon, title) = if is_permanent {
            ("🛑", "Sync failed — permanent error")
        } else {
            ("❌", "Sync failed")
        };

        // Truncate error message for card display
        let short_error = truncate_for_card(error, 300);

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
        let repo_name = event
            .get("repo_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Repository");

        match phase {
            "completed" => {
                let total_revs = event
                    .get("total_revs")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                let commits = event
                    .get("commits_created")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);

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
                let short_error = truncate_for_card(error, 300);

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
pub fn format_branch_created(
    repo_name: &str,
    git_branch: &str,
    svn_branch: &str,
) -> serde_json::Value {
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
pub fn format_path_violation(
    repo_name: &str,
    violations: &[String],
    skipped_all: bool,
) -> serde_json::Value {
    let (icon, title) = if skipped_all {
        ("⚠️", "Commit skipped — all files violate path rules")
    } else {
        ("⚠️", "Files filtered from commit")
    };

    let violation_text = if violations.len() > 5 {
        let shown: Vec<&str> = violations.iter().take(5).map(|s| s.as_str()).collect();
        format!(
            "{}  \nand {} more",
            shown.join("  \n"),
            violations.len() - 5
        )
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
                "Sync has been paused after {} consecutive permanent errors. Use **Skip Commit** to exclude selected pending commits at observed tips, or **Retry** if the issue has been resolved. Skip does not adopt live HEAD.",
                consecutive_errors
            ),
            "wrap": true,
            "color": "Attention"
        }
    ])
}

/// Preserve the existing byte budgets while backing up to a UTF-8 boundary.
fn truncate_for_card(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Forward broadcast events to Teams for as long as the channel is open.
///
/// `webhook_url` is asked for the current address before each event, so a
/// URL saved, replaced or removed in Settings applies to the next event
/// without a restart. `None` (or an empty URL) sends nothing.
pub async fn run_listener<F>(mut events: tokio::sync::broadcast::Receiver<String>, webhook_url: F)
where
    F: Fn() -> Option<String>,
{
    use tokio::sync::broadcast::error::RecvError;
    let mut current: Option<TeamsNotifier> = None;
    loop {
        let msg = match events.recv().await {
            Ok(msg) => msg,
            Err(RecvError::Lagged(n)) => {
                warn!(skipped = n, "Teams listener lagged, missed events");
                continue;
            }
            Err(RecvError::Closed) => {
                info!("Teams listener: broadcast channel closed, shutting down");
                break;
            }
        };
        let Some(url) = webhook_url().filter(|u| !u.trim().is_empty()) else {
            current = None;
            continue;
        };
        if current.as_ref().map(|n| n.webhook_url.as_str()) != Some(url.as_str()) {
            current = Some(TeamsNotifier::new(url));
        }
        if let Some(notifier) = &current {
            notifier.process_event(&msg).await;
        }
    }
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
        let body = card[1]["text"].as_str().unwrap();
        assert!(body.contains("Skip Commit"));
        assert!(body.contains("does not adopt live HEAD"));
    }

    #[test]
    fn test_format_path_violation_many_files() {
        let violations: Vec<String> = (0..8).map(|i| format!("bad/file{}.txt", i)).collect();
        let card = format_path_violation("Test Repo", &violations, false);
        let card_str = card.to_string();
        assert!(card_str.contains("and 3 more"));
    }

    #[test]
    fn candidate_228_unicode_crossing_all_card_byte_limits_is_safe() {
        let notifier = TeamsNotifier::new("https://example.invalid".into());
        for character in ["é", "界", "😀"] {
            let prefix = "a".repeat(119);
            let rich = notifier
                .format_sync_completed(&json!({
                    "git_to_svn":1,"commits":[{"message":format!("{prefix}{character}tail\nbody") }]
                }))
                .unwrap();
            assert!(rich.to_string().contains(&format!("{prefix}…")));
            assert!(!rich.to_string().contains("tail"));
            let prefix = "a".repeat(99);
            let legacy = notifier
                .format_sync_completed(&json!({
                    "svn_to_git":1,"messages":[format!("{prefix}{character}tail")]
                }))
                .unwrap();
            assert!(legacy.to_string().contains(&format!("• {prefix}…")));
            let prefix = "a".repeat(299);
            let error = format!("{prefix}{character}tail");
            let failed = notifier.format_sync_failed(&json!({"error":error}));
            assert!(failed.to_string().contains(&format!("{prefix}…")));
            let import = notifier
                .format_import_progress(&json!({"phase":"failed","error":error}))
                .unwrap();
            assert!(import.to_string().contains(&format!("{prefix}…")));
        }
    }

    #[test]
    fn candidate_228_ascii_and_short_unicode_display_remain_unchanged() {
        for limit in [100, 120, 300] {
            assert_eq!(truncate_for_card("é界😀", limit), "é界😀");
            assert_eq!(
                truncate_for_card(&"x".repeat(limit), limit),
                "x".repeat(limit)
            );
            assert_eq!(
                truncate_for_card(&"x".repeat(limit + 1), limit),
                format!("{}…", "x".repeat(limit))
            );
        }
    }
    /// Accept HTTP requests on a loopback listener and report each body.
    async fn capture() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf).to_string();
                        if let Some((head, body)) = text.split_once("\r\n\r\n") {
                            let len = head
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if body.len() >= len {
                                let _ = stream
                                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                                    .await;
                                let _ = tx.send(body.to_string());
                                return;
                            }
                        }
                    }
                });
            }
        });
        (url, rx)
    }

    #[tokio::test]
    async fn a_failed_send_does_not_reveal_the_webhook_url() {
        // Nothing listens on port 9 of the loopback address.
        let notifier = TeamsNotifier::new("http://127.0.0.1:9/hook?sig=secret-token".into());
        let err = notifier.send_card(json!([])).await.unwrap_err().to_string();
        assert!(!err.contains("secret-token"), "{err}");
        assert!(!err.contains("127.0.0.1:9"), "{err}");
    }

    fn failed_event(repo: &str) -> String {
        json!({"type":"repo_sync_failed","repo_name":repo,"error":"boom"}).to_string()
    }

    async fn next(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no Teams delivery")
            .unwrap()
    }

    #[tokio::test]
    async fn listener_follows_the_current_url_without_a_restart() {
        let (first, mut first_rx) = capture().await;
        let (second, mut second_rx) = capture().await;
        let url = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = tokio::sync::broadcast::channel(16);
        let (source, counter) = (url.clone(), asked.clone());
        let task = tokio::spawn(run_listener(rx, move || {
            let current = source.lock().unwrap().clone();
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            current
        }));
        // Wait until the listener has looked up the URL for `n` events, so a
        // change made after that applies only to later events.
        let handled = |n: usize| {
            let asked = asked.clone();
            async move {
                for _ in 0..500 {
                    if asked.load(std::sync::atomic::Ordering::SeqCst) >= n {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                panic!("listener did not handle event {n}");
            }
        };

        // No URL: nothing is sent.
        tx.send(failed_event("none-set")).unwrap();
        handled(1).await;
        *url.lock().unwrap() = Some(first.clone());
        tx.send(failed_event("alpha")).unwrap();
        let body = next(&mut first_rx).await;
        assert!(body.contains("alpha"), "{body}");

        // A replaced URL takes effect on the next event.
        *url.lock().unwrap() = Some(second.clone());
        tx.send(failed_event("beta")).unwrap();
        assert!(next(&mut second_rx).await.contains("beta"));

        // A removed URL stops delivery; a filtered event sends nothing.
        *url.lock().unwrap() = Some(String::new());
        tx.send(failed_event("cleared")).unwrap();
        handled(4).await;
        *url.lock().unwrap() = Some(first.clone());
        tx.send(
            json!({"type":"repo_sync_completed","repo_name":"quiet","svn_to_git":0,"git_to_svn":0})
                .to_string(),
        )
        .unwrap();
        tx.send(failed_event("gamma")).unwrap();
        let body = next(&mut first_rx).await;
        assert!(body.contains("gamma"), "{body}");
        assert!(first_rx.try_recv().is_err());
        assert!(second_rx.try_recv().is_err());
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("listener stops when the channel closes")
            .unwrap();
    }

    // R17: notifications use one global channel. Interleaved events from two
    // pairs each produce one card in that channel, naming only their own pair.
    #[tokio::test]
    async fn interleaved_pair_events_each_post_one_card_naming_their_pair() {
        let (channel, mut rx) = capture().await;
        let (tx, events) = tokio::sync::broadcast::channel(16);
        let url = channel.clone();
        let task = tokio::spawn(run_listener(events, move || Some(url.clone())));
        let completed = |name: &str| {
            json!({"type":"repo_sync_completed","repo_name":name,"svn_to_git":1,"git_to_svn":0})
                .to_string()
        };
        for event in [
            completed("pair-alpha"),
            failed_event("pair-beta"),
            completed("pair-beta"),
            failed_event("pair-alpha"),
        ] {
            tx.send(event).unwrap();
        }
        // The listener posts serially, but the capture server hands bodies
        // over from one task per connection, so arrival order is not fixed.
        let mut seen = std::collections::BTreeMap::new();
        for _ in 0..4 {
            let body = next(&mut rx).await;
            let alpha = body.contains("pair-alpha");
            let beta = body.contains("pair-beta");
            assert!(alpha != beta, "each card names exactly one pair: {body}");
            let own = if alpha { "pair-alpha" } else { "pair-beta" };
            let failed = body.contains("Sync failed");
            assert!(
                seen.insert((own, failed), ()).is_none(),
                "one {} card for {own}: {body}",
                if failed { "failed" } else { "completed" }
            );
        }
        assert_eq!(seen.len(), 4, "a completed and a failed card per pair");
        drop(tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("listener stops when the channel closes")
            .unwrap();
        assert!(rx.try_recv().is_err(), "one card per event");
    }
}
