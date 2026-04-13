//! Notification subsystem for conflict alerts, sync errors, and summaries.
//!
//! Supports Slack webhook and SMTP email channels. The [`Notifier`] facade
//! dispatches to all configured channels and logs failures without aborting.

pub mod email;
pub mod slack;
pub mod teams;

use tracing::{info, warn};

use crate::config::NotificationConfig;
use crate::conflict::Conflict;
use crate::errors::NotificationError;
use crate::sync_engine::SyncStats;

/// Unified notifier that dispatches to all configured channels.
pub struct Notifier {
    slack: Option<slack::SlackNotifier>,
    email: Option<email::EmailNotifier>,
    pub teams: Option<teams::TeamsNotifier>,
}

impl Notifier {
    /// Create a new notifier from the notification configuration.
    pub fn new(config: &NotificationConfig) -> Self {
        let slack = config.slack_webhook_url.as_ref().map(|url| {
            info!("Slack notifications enabled");
            slack::SlackNotifier::new(url.clone())
        });

        let email = match (&config.email_smtp, &config.email_from) {
            (Some(smtp), Some(from)) if !config.email_recipients.is_empty() => {
                info!("email notifications enabled");
                Some(email::EmailNotifier::new(
                    smtp.clone(),
                    from.clone(),
                    config.email_recipients.clone(),
                ))
            }
            _ => None,
        };

        let teams = config.teams_webhook_url.as_ref().map(|url| {
            info!("Teams notifications enabled");
            teams::TeamsNotifier::new(url.clone())
        });

        Self { slack, email, teams }
    }

    /// Send a conflict notification to all configured channels.
    pub async fn notify_conflict(&self, conflict: &Conflict) -> Result<(), NotificationError> {
        info!(
            file = %conflict.file_path,
            conflict_type = %conflict.conflict_type,
            "sending conflict notification"
        );

        let mut errors = Vec::new();

        if let Some(ref slack) = self.slack {
            let message = format_conflict_slack(conflict);
            if let Err(e) = slack.send_message(&message).await {
                warn!(error = %e, "Slack notification failed");
                errors.push(format!("Slack: {}", e));
            }
        }

        if let Some(ref email) = self.email {
            let subject = format!("[RepoSync] Conflict detected: {}", conflict.file_path);
            let body = format_conflict_email_html(conflict);
            if let Err(e) = email.send(&subject, &body).await {
                warn!(error = %e, "email notification failed");
                errors.push(format!("Email: {}", e));
            }
        }

        if !errors.is_empty() && (self.slack.is_some() || self.email.is_some()) {
            // Only error if ALL channels failed.
            let has_slack = self.slack.is_some();
            let has_email = self.email.is_some();
            let total_channels = has_slack as usize + has_email as usize;
            if errors.len() >= total_channels {
                return Err(NotificationError::AllChannelsFailed(errors.join("; ")));
            }
        }

        Ok(())
    }

    /// Send a sync error notification to all configured channels.
    pub async fn notify_sync_error(&self, error: &str) -> Result<(), NotificationError> {
        info!("sending sync error notification");

        if let Some(ref slack) = self.slack {
            let message = format!(":x: *RepoSync Error*\n```{}```", error);
            let _ = slack.send_message(&message).await;
        }

        if let Some(ref email) = self.email {
            let subject = "[RepoSync] Sync Error";
            let body = format!(
                "<html><body>\
                <h2 style=\"color: red;\">RepoSync Sync Error</h2>\
                <pre>{}</pre>\
                </body></html>",
                html_escape(error)
            );
            let _ = email.send(subject, &body).await;
        }

        Ok(())
    }

    /// Send a sync-complete summary notification (optional).
    pub async fn notify_sync_complete(&self, stats: &SyncStats) -> Result<(), NotificationError> {
        // Only send if there were actual changes.
        if stats.svn_to_git_count == 0
            && stats.git_to_svn_count == 0
            && stats.conflicts_detected == 0
        {
            return Ok(());
        }

        info!("sending sync completion notification");

        if let Some(ref slack) = self.slack {
            let message = format!(
                ":white_check_mark: *RepoSync Cycle Complete*\n\
                 - SVN -> Git: {} commits\n\
                 - Git -> SVN: {} commits\n\
                 - Conflicts: {} ({} auto-resolved)",
                stats.svn_to_git_count,
                stats.git_to_svn_count,
                stats.conflicts_detected,
                stats.conflicts_auto_resolved,
            );
            let _ = slack.send_message(&message).await;
        }

        if let Some(ref email) = self.email {
            let subject = "[RepoSync] Sync Complete";
            let body = format!(
                "<html><body>\
                <h2>RepoSync Sync Cycle Complete</h2>\
                <table>\
                <tr><td>SVN -&gt; Git</td><td>{}</td></tr>\
                <tr><td>Git -&gt; SVN</td><td>{}</td></tr>\
                <tr><td>Conflicts</td><td>{}</td></tr>\
                <tr><td>Auto-resolved</td><td>{}</td></tr>\
                </table>\
                </body></html>",
                stats.svn_to_git_count,
                stats.git_to_svn_count,
                stats.conflicts_detected,
                stats.conflicts_auto_resolved,
            );
            let _ = email.send(subject, &body).await;
        }

        Ok(())
    }

    /// Return whether any notification channel is configured.
    pub fn is_configured(&self) -> bool {
        self.slack.is_some() || self.email.is_some()
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a conflict notification for Slack (Markdown).
fn format_conflict_slack(conflict: &Conflict) -> String {
    let mut msg = format!(
        ":warning: *Sync Conflict Detected*\n\
         *File:* `{}`\n\
         *Type:* {}\n\
         *Status:* {}",
        conflict.file_path, conflict.conflict_type, conflict.status,
    );

    if let Some(rev) = conflict.svn_rev {
        msg.push_str(&format!("\n*SVN Revision:* r{}", rev));
    }
    if let Some(ref sha) = conflict.git_sha {
        msg.push_str(&format!("\n*Git SHA:* `{}`", &sha[..8.min(sha.len())]));
    }

    msg.push_str("\n\nPlease resolve this conflict in the RepoSync dashboard.");
    msg
}

/// Format a conflict notification as an HTML email.
fn format_conflict_email_html(conflict: &Conflict) -> String {
    let mut html = format!(
        "<html><body>\
        <h2 style=\"color: #d4a017;\">Sync Conflict Detected</h2>\
        <table style=\"border-collapse: collapse;\">\
        <tr><td style=\"padding: 4px 12px; font-weight: bold;\">File</td>\
            <td style=\"padding: 4px 12px;\"><code>{}</code></td></tr>\
        <tr><td style=\"padding: 4px 12px; font-weight: bold;\">Type</td>\
            <td style=\"padding: 4px 12px;\">{}</td></tr>\
        <tr><td style=\"padding: 4px 12px; font-weight: bold;\">Status</td>\
            <td style=\"padding: 4px 12px;\">{}</td></tr>",
        html_escape(&conflict.file_path),
        html_escape(&conflict.conflict_type.to_string()),
        html_escape(&conflict.status.to_string()),
    );

    if let Some(rev) = conflict.svn_rev {
        html.push_str(&format!(
            "<tr><td style=\"padding: 4px 12px; font-weight: bold;\">SVN Rev</td>\
             <td style=\"padding: 4px 12px;\">r{}</td></tr>",
            rev
        ));
    }
    if let Some(ref sha) = conflict.git_sha {
        html.push_str(&format!(
            "<tr><td style=\"padding: 4px 12px; font-weight: bold;\">Git SHA</td>\
             <td style=\"padding: 4px 12px;\"><code>{}</code></td></tr>",
            html_escape(&sha[..8.min(sha.len())])
        ));
    }

    html.push_str("</table>");
    html.push_str("<p>Please resolve this conflict in the RepoSync dashboard.</p>");
    html.push_str("</body></html>");
    html
}

/// Minimal HTML escaping for user-provided strings.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationConfig;
    use crate::conflict::detector::{ConflictStatus, ConflictType};

    #[test]
    fn test_notifier_not_configured() {
        let config = NotificationConfig::default();
        let notifier = Notifier::new(&config);
        assert!(!notifier.is_configured());
    }

    #[test]
    fn test_format_conflict_slack() {
        let conflict = Conflict {
            id: "test-id".into(),
            file_path: "src/main.rs".into(),
            conflict_type: ConflictType::Content,
            svn_content: None,
            git_content: None,
            base_content: None,
            svn_rev: Some(42),
            git_sha: Some("abc12345".into()),
            status: ConflictStatus::Detected,
            resolution: None,
            resolved_by: None,
        };

        let msg = format_conflict_slack(&conflict);
        assert!(msg.contains("src/main.rs"));
        assert!(msg.contains("r42"));
        assert!(msg.contains("abc12345"));
    }

    #[test]
    fn test_format_conflict_email() {
        let conflict = Conflict {
            id: "test-id".into(),
            file_path: "lib/util.rs".into(),
            conflict_type: ConflictType::EditDelete,
            svn_content: None,
            git_content: None,
            base_content: None,
            svn_rev: None,
            git_sha: None,
            status: ConflictStatus::Detected,
            resolution: None,
            resolved_by: None,
        };

        let html = format_conflict_email_html(&conflict);
        assert!(html.contains("lib/util.rs"));
        assert!(html.contains("edit_delete"));
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }
}
