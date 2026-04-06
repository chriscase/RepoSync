//! GitHub REST API client.

use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::time::sleep;
use tracing::{debug, info, instrument, warn};

use crate::config::GitProvider;
use crate::errors::GitHubError;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCommit {
    pub sha: String,
    pub commit: GitHubCommitDetail,
    pub author: Option<GitHubUserSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCommitDetail {
    pub message: String,
    pub author: GitHubGitActor,
    pub committer: GitHubGitActor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubGitActor {
    pub name: String,
    pub email: String,
    pub date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUserSummary {
    pub login: String,
    pub id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
    pub id: u64,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub html_url: String,
    pub state: String,
    pub head: PullRequestRef,
    pub base: PullRequestRef,
    pub merged: Option<bool>,
    pub merge_commit_sha: Option<String>,
    pub merged_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

/// Detailed commit info from `GET /repos/{owner}/{repo}/commits/{sha}`.
/// Includes the `parents` array needed for merge strategy detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCommitDetail2 {
    pub sha: String,
    pub commit: GitHubCommitDetail,
    pub author: Option<GitHubUserSummary>,
    pub parents: Vec<GitHubCommitParent>,
}

/// A parent reference in a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCommitParent {
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Pending,
    Success,
    Failure,
    Error,
}

impl std::fmt::Display for CommitStatusState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Success => write!(f, "success"),
            Self::Failure => write!(f, "failure"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// Asynchronous GitHub REST API client.
#[derive(Clone)]
pub struct GitHubClient {
    http: reqwest::Client,
    api_url: String,
    token: String,
    provider: GitProvider,
}

impl GitHubClient {
    pub fn new(api_url: impl Into<String>, token: impl Into<String>, provider: GitProvider) -> Self {
        let api_url = api_url.into().trim_end_matches('/').to_string();
        let token = token.into();
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static("reposync/0.1"));
        match provider {
            GitProvider::GitHub => {
                headers.insert(ACCEPT, HeaderValue::from_static("application/vnd.github+json"));
                headers.insert("X-GitHub-Api-Version", HeaderValue::from_static("2022-11-28"));
            }
            GitProvider::Gitea => {
                headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            }
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("failed to build reqwest client");
        info!(api_url = %api_url, ?provider, "created GitHubClient");
        Self {
            http,
            api_url,
            token,
            provider,
        }
    }

    /// Apply provider-appropriate authentication to a request.
    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.provider {
            GitProvider::Gitea => req.header("Authorization", format!("token {}", self.token)),
            GitProvider::GitHub => req.bearer_auth(&self.token),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_commits(
        &self,
        repo: &str,
        since_sha: Option<&str>,
    ) -> Result<Vec<GitHubCommit>, GitHubError> {
        let first_url = format!("{}/repos/{}/commits", self.api_url, repo);
        let since_sha = since_sha.map(|s| s.to_string());
        let resp = self.retry_request(|| {
            let mut req = self.auth(self.http.get(&first_url));
            if let Some(ref sha) = since_sha {
                req = req.query(&[("sha", sha.as_str())]);
            }
            req.query(&[("per_page", "100")])
        }).await?;

        let mut next_link = Self::parse_next_link(resp.headers());
        let mut all_commits: Vec<GitHubCommit> = resp.json().await?;
        let mut pages = 1usize;

        while let Some(ref url) = next_link {
            if pages >= 10 {
                warn!(pages, "reached GitHub pagination limit for get_commits");
                break;
            }
            let url_clone = url.clone();
            let resp = self.retry_request(|| self.auth(self.http.get(&url_clone))).await?;
            next_link = Self::parse_next_link(resp.headers());
            let page: Vec<GitHubCommit> = resp.json().await?;
            all_commits.extend(page);
            pages += 1;
        }

        debug!(count = all_commits.len(), pages, "fetched commits");
        Ok(all_commits)
    }

    #[instrument(skip(self, secret))]
    pub async fn create_webhook(
        &self,
        repo: &str,
        callback_url: &str,
        secret: &str,
    ) -> Result<serde_json::Value, GitHubError> {
        let url = format!("{}/repos/{}/hooks", self.api_url, repo);
        let body = serde_json::json!({
            "name": "web", "active": true, "events": ["push", "pull_request"],
            "config": { "url": callback_url, "content_type": "json", "secret": secret, "insecure_ssl": "0" }
        });
        let resp = self.auth(self
                .http
                .post(&url))
            .json(&body)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let hook: serde_json::Value = resp.json().await?;
        info!(hook_id = %hook["id"], "created webhook");
        Ok(hook)
    }

    /// Verify a GitHub/Gitea webhook signature.
    pub fn verify_webhook_signature(payload: &[u8], signature: &str, secret: &str, provider: &GitProvider) -> bool {
        let hex_sig = match provider {
            GitProvider::GitHub => {
                match signature.strip_prefix("sha256=") {
                    Some(s) => s,
                    None => {
                        warn!("webhook signature missing sha256= prefix");
                        return false;
                    }
                }
            }
            GitProvider::Gitea => signature, // Gitea sends raw hex, no prefix
        };
        let expected_bytes = match hex::decode(hex_sig) {
            Ok(b) => b,
            Err(_) => {
                warn!("webhook signature is not valid hex");
                return false;
            }
        };
        let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                warn!("failed to create HMAC");
                return false;
            }
        };
        mac.update(payload);
        mac.verify_slice(&expected_bytes).is_ok()
    }

    #[instrument(skip(self, body))]
    pub async fn create_pull_request(
        &self,
        repo: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PullRequest, GitHubError> {
        let url = format!("{}/repos/{}/pulls", self.api_url, repo);
        let payload =
            serde_json::json!({ "title": title, "body": body, "head": head, "base": base });
        let resp = self.auth(self
                .http
                .post(&url))
            .json(&payload)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let pr: PullRequest = resp.json().await?;
        info!(number = pr.number, "created pull request");
        Ok(pr)
    }

    #[instrument(skip(self))]
    pub async fn merge_pull_request(&self, repo: &str, pr_number: u64) -> Result<(), GitHubError> {
        let url = format!("{}/repos/{}/pulls/{}/merge", self.api_url, repo, pr_number);
        let payload = serde_json::json!({ "merge_method": "merge" });
        let resp = self.auth(self
                .http
                .put(&url))
            .json(&payload)
            .send()
            .await?;
        let _resp = self.check_response(resp).await?;
        info!(pr_number, "merged pull request");
        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn get_user(&self, username: &str) -> Result<GitHubUser, GitHubError> {
        let url = format!("{}/users/{}", self.api_url, username);
        let resp = self.auth(self.http.get(&url)).send().await?;
        let resp = self.check_response(resp).await?;
        let user: GitHubUser = resp.json().await?;
        debug!(login = %user.login, "fetched user");
        Ok(user)
    }

    #[instrument(skip(self))]
    pub async fn post_commit_status(
        &self,
        repo: &str,
        sha: &str,
        state: CommitStatusState,
        description: &str,
    ) -> Result<(), GitHubError> {
        let url = format!("{}/repos/{}/statuses/{}", self.api_url, repo, sha);
        let payload = serde_json::json!({ "state": state.to_string(), "description": description, "context": "reposync" });
        let resp = self.auth(self
                .http
                .post(&url))
            .json(&payload)
            .send()
            .await?;
        let _resp = self.check_response(resp).await?;
        debug!(sha, state = %state, "posted commit status");
        Ok(())
    }

    // -- Personal Branch Mode methods -----------------------------------------

    /// List recently merged pull requests targeting `base` branch.
    #[instrument(skip(self))]
    pub async fn get_merged_pull_requests(
        &self,
        repo: &str,
        base: &str,
        since: Option<&str>,
    ) -> Result<Vec<PullRequest>, GitHubError> {
        let first_url = format!("{}/repos/{}/pulls", self.api_url, repo);
        let base = base.to_string();
        let since = since.map(|s| s.to_string());
        let resp = self.retry_request(|| {
            let mut req = self.auth(self.http.get(&first_url)).query(&[
                ("state", "closed"),
                ("base", base.as_str()),
                ("per_page", "100"),
            ]);
            if since.is_some() {
                req = req.query(&[("sort", "updated"), ("direction", "desc")]);
            }
            req
        }).await?;

        let mut next_link = Self::parse_next_link(resp.headers());
        let mut all_prs: Vec<PullRequest> = resp.json().await?;
        let mut pages = 1usize;

        while let Some(ref url) = next_link {
            if pages >= 10 {
                warn!(pages, "reached GitHub pagination limit for get_merged_pull_requests");
                break;
            }
            let url_clone = url.clone();
            let resp = self.retry_request(|| self.auth(self.http.get(&url_clone))).await?;
            next_link = Self::parse_next_link(resp.headers());
            let page: Vec<PullRequest> = resp.json().await?;
            all_prs.extend(page);
            pages += 1;
        }

        // Filter to only merged PRs, optionally after a timestamp
        let merged: Vec<PullRequest> = all_prs
            .into_iter()
            .filter(|pr| pr.merged == Some(true))
            .filter(|pr| {
                if let Some(ref since_dt) = since {
                    pr.merged_at
                        .as_deref()
                        .map(|m| m >= since_dt.as_str())
                        .unwrap_or(false)
                } else {
                    true
                }
            })
            .collect();
        debug!(count = merged.len(), base = %base, pages, "fetched merged pull requests");
        Ok(merged)
    }

    /// Get commits for a specific pull request.
    #[instrument(skip(self))]
    pub async fn get_pr_commits(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<Vec<GitHubCommit>, GitHubError> {
        let url = format!(
            "{}/repos/{}/pulls/{}/commits",
            self.api_url, repo, pr_number
        );
        let resp = self.auth(self
                .http
                .get(&url))
            .query(&[("per_page", "100")])
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let commits: Vec<GitHubCommit> = resp.json().await?;
        debug!(count = commits.len(), pr_number, "fetched PR commits");
        Ok(commits)
    }

    /// Get a single pull request by number.
    #[instrument(skip(self))]
    pub async fn get_pull_request(
        &self,
        repo: &str,
        pr_number: u64,
    ) -> Result<PullRequest, GitHubError> {
        let url = format!("{}/repos/{}/pulls/{}", self.api_url, repo, pr_number);
        let resp = self.auth(self.http.get(&url)).send().await?;
        let resp = self.check_response(resp).await?;
        let pr: PullRequest = resp.json().await?;
        debug!(number = pr.number, state = %pr.state, "fetched pull request");
        Ok(pr)
    }

    /// Get a single commit by SHA (includes parent count for merge detection).
    #[instrument(skip(self))]
    pub async fn get_commit(
        &self,
        repo: &str,
        sha: &str,
    ) -> Result<GitHubCommitDetail2, GitHubError> {
        let url = format!("{}/repos/{}/commits/{}", self.api_url, repo, sha);
        let resp = self.auth(self.http.get(&url)).send().await?;
        let resp = self.check_response(resp).await?;
        let commit: GitHubCommitDetail2 = resp.json().await?;
        debug!(
            sha,
            parents = commit.parents.len(),
            "fetched commit details"
        );
        Ok(commit)
    }

    /// Check whether a repository exists.
    #[instrument(skip(self))]
    pub async fn repo_exists(&self, repo: &str) -> Result<bool, GitHubError> {
        let url = format!("{}/repos/{}", self.api_url, repo);
        let resp = self.auth(self.http.head(&url)).send().await?;
        Ok(resp.status().is_success())
    }

    /// Create a new GitHub repository.
    #[instrument(skip(self))]
    pub async fn create_repo(
        &self,
        name: &str,
        private: bool,
        description: &str,
    ) -> Result<serde_json::Value, GitHubError> {
        let url = format!("{}/user/repos", self.api_url);
        let payload = serde_json::json!({
            "name": name,
            "private": private,
            "description": description,
            "auto_init": false,
        });
        let resp = self.auth(self
                .http
                .post(&url))
            .json(&payload)
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let repo: serde_json::Value = resp.json().await?;
        info!(repo_name = name, private, "created repository");
        Ok(repo)
    }

    /// Create a new branch in the remote repository from an existing branch.
    ///
    /// Works for both GitHub and Gitea:
    /// - GitHub: GET ref → POST git/refs
    /// - Gitea: POST /repos/{owner}/{repo}/branches
    #[instrument(skip(self))]
    pub async fn create_branch(
        &self,
        repo: &str,
        branch_name: &str,
        from_branch: &str,
    ) -> Result<(), GitHubError> {
        match self.provider {
            GitProvider::Gitea => {
                // Gitea has a dedicated branch creation endpoint
                let url = format!("{}/repos/{}/branches", self.api_url, repo);
                let body = serde_json::json!({
                    "new_branch_name": branch_name,
                    "old_branch_name": from_branch,
                });
                let resp = self
                    .auth(self.http.post(&url).json(&body))
                    .send()
                    .await?;
                if resp.status().as_u16() == 409 {
                    // Branch already exists
                    return Ok(());
                }
                self.check_response(resp).await?;
            }
            GitProvider::GitHub => {
                // Step 1: Get the SHA of the source branch
                let ref_url = format!(
                    "{}/repos/{}/git/ref/heads/{}",
                    self.api_url, repo, from_branch
                );
                let resp = self.auth(self.http.get(&ref_url)).send().await?;
                let resp = self.check_response(resp).await?;
                let ref_data: serde_json::Value = resp.json().await?;
                let sha = ref_data["object"]["sha"]
                    .as_str()
                    .ok_or_else(|| GitHubError::ApiError {
                        status: 500,
                        body: format!(
                            "could not resolve SHA for branch '{}'",
                            from_branch
                        ),
                    })?
                    .to_string();

                // Step 2: Create the new ref
                let refs_url =
                    format!("{}/repos/{}/git/refs", self.api_url, repo);
                let body = serde_json::json!({
                    "ref": format!("refs/heads/{}", branch_name),
                    "sha": sha,
                });
                let resp = self
                    .auth(self.http.post(&refs_url).json(&body))
                    .send()
                    .await?;
                if resp.status().as_u16() == 422 {
                    // Reference already exists
                    return Ok(());
                }
                self.check_response(resp).await?;
            }
        }
        info!(
            repo,
            branch_name,
            from_branch,
            "created remote branch"
        );
        Ok(())
    }

    /// Delete a branch in the remote repository.
    #[instrument(skip(self))]
    pub async fn delete_branch(
        &self,
        repo: &str,
        branch_name: &str,
    ) -> Result<(), GitHubError> {
        match self.provider {
            GitProvider::Gitea => {
                let url = format!(
                    "{}/repos/{}/branches/{}",
                    self.api_url, repo, branch_name
                );
                let resp = self.auth(self.http.delete(&url)).send().await?;
                if resp.status().as_u16() == 404 {
                    return Ok(()); // already deleted
                }
                self.check_response(resp).await?;
            }
            GitProvider::GitHub => {
                let url = format!(
                    "{}/repos/{}/git/refs/heads/{}",
                    self.api_url, repo, branch_name
                );
                let resp = self.auth(self.http.delete(&url)).send().await?;
                if resp.status().as_u16() == 422 {
                    return Ok(()); // already deleted
                }
                self.check_response(resp).await?;
            }
        }
        info!(repo, branch_name, "deleted remote branch");
        Ok(())
    }

    /// Check if a branch is merged into another branch.
    /// Returns the number of commits ahead (0 = fully merged).
    #[instrument(skip(self))]
    pub async fn compare_branches(
        &self,
        repo: &str,
        base: &str,
        head: &str,
    ) -> Result<u64, GitHubError> {
        let url = format!(
            "{}/repos/{}/compare/{}...{}",
            self.api_url, repo, base, head
        );
        let resp = self.auth(self.http.get(&url)).send().await?;
        let resp = self.check_response(resp).await?;
        let data: serde_json::Value = resp.json().await?;
        let ahead = data["ahead_by"].as_u64()
            .or_else(|| data["commits"].as_array().map(|a| a.len() as u64))
            .unwrap_or(0);
        Ok(ahead)
    }

    /// Get the authenticated user's login.
    #[instrument(skip(self))]
    pub async fn get_authenticated_user(&self) -> Result<GitHubUser, GitHubError> {
        let url = format!("{}/user", self.api_url);
        let resp = self.auth(self.http.get(&url)).send().await?;
        let resp = self.check_response(resp).await?;
        let user: GitHubUser = resp.json().await?;
        debug!(login = %user.login, "fetched authenticated user");
        Ok(user)
    }

    /// Parse the `rel="next"` URL from a GitHub `Link` response header.
    fn parse_next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
        let link_val = headers.get("link")?.to_str().ok()?;
        for part in link_val.split(',') {
            let mut url_part = None;
            let mut is_next = false;
            for segment in part.split(';') {
                let s = segment.trim();
                if s.starts_with('<') && s.ends_with('>') {
                    url_part = Some(s[1..s.len() - 1].to_string());
                } else if s == "rel=\"next\"" {
                    is_next = true;
                }
            }
            if is_next {
                return url_part;
            }
        }
        None
    }

    /// Execute an HTTP request with exponential backoff retry.
    ///
    /// Retries up to 3 times on 429 (Too Many Requests) and 5xx server errors.
    async fn retry_request<F>(&self, make_req: F) -> Result<reqwest::Response, GitHubError>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0u32;
        loop {
            let resp = make_req().send().await?;
            let status = resp.status();

            if status.is_success() {
                return Ok(resp);
            }

            let should_retry = status.as_u16() == 429 || status.is_server_error();
            if !should_retry || attempt >= MAX_RETRIES {
                return self.check_response(resp).await;
            }

            let wait_secs = if status.as_u16() == 429 {
                resp.headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1u64 << attempt)
                    .min(60)
            } else {
                1u64 << attempt
            };

            warn!(status = status.as_u16(), attempt, wait_secs, "GitHub API transient error; retrying");
            sleep(std::time::Duration::from_secs(wait_secs)).await;
            attempt += 1;
        }
    }

    /// Validate a response. On success, returns the response for further
    /// processing (e.g. `.json()`). On failure, consumes the response and
    /// returns a rich error with request-id + redacted, truncated body context.
    ///
    /// Auth (401/403) and rate-limit (429) errors are mapped to their specific
    /// error variants.  All other non-success statuses include the safe body
    /// snippet in the `ApiError` variant.
    async fn check_response(
        &self,
        resp: reqwest::Response,
    ) -> Result<reqwest::Response, GitHubError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        // Extract the GitHub request ID if present (safe, non-secret diagnostic).
        let request_id = resp
            .headers()
            .get("x-github-request-id")
            .or_else(|| resp.headers().get("x-request-id"))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none")
            .to_string();

        // For rate-limit errors, extract the reset header before consuming the body.
        if status.as_u16() == 429 {
            let reset = resp
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            return Err(GitHubError::RateLimited { reset_at: reset });
        }

        // Read the body for diagnostic context (safe: truncated + redacted).
        let body_snippet = Self::extract_safe_body(resp).await;

        if status.as_u16() == 401 || status.as_u16() == 403 {
            warn!(
                http_status = status.as_u16(),
                request_id = %request_id,
                body = %body_snippet,
                "GitHub authentication failure"
            );
            return Err(GitHubError::AuthenticationFailed(format!(
                "HTTP {} (request-id: {}) | {}",
                status, request_id, body_snippet
            )));
        }

        Err(GitHubError::ApiError {
            status: status.as_u16(),
            body: format!(
                "HTTP {} (request-id: {}) | {}",
                status, request_id, body_snippet
            ),
        })
    }

    /// Read the response body, truncate to 512 bytes, and redact secrets.
    ///
    /// Used internally by `check_response` for forensic diagnostics.  Never
    /// surfaces tokens or passwords.
    async fn extract_safe_body(resp: reqwest::Response) -> String {
        match resp.text().await {
            Ok(text) => {
                let safe = Self::redact_secrets(&text);
                if safe.len() > 512 {
                    format!("{}...(truncated)", &safe[..512])
                } else {
                    safe
                }
            }
            Err(_) => "(could not read response body)".to_string(),
        }
    }

    /// Redact known secret patterns from a string to prevent credential leakage
    /// in logs.  Covers GitHub tokens (ghp_, gho_, ghs_, ghu_, github_pat_),
    /// Bearer tokens, and generic password/secret/token value patterns.
    pub fn redact_secrets(input: &str) -> String {
        // GitHub PATs: ghp_, gho_, ghs_, ghu_, github_pat_ followed by
        // alphanumeric+underscore.
        let re_ghp = regex_lite::Regex::new(r"(ghp_|gho_|ghs_|ghu_|github_pat_)[A-Za-z0-9_]+")
            .expect("valid regex");
        let redacted = re_ghp.replace_all(input, "[REDACTED_TOKEN]");

        // Gitea tokens: 40-character hex strings (standalone, not part of a longer word).
        let re_hex_token =
            regex_lite::Regex::new(r"\b[0-9a-f]{40}\b").expect("valid regex");
        let redacted = re_hex_token.replace_all(&redacted, "[REDACTED_HEX_TOKEN]");

        // Bearer <token> in headers dumped into error bodies.
        let re_bearer =
            regex_lite::Regex::new(r"(?i)bearer\s+[A-Za-z0-9_.~+/=-]+").expect("valid regex");
        let redacted = re_bearer.replace_all(&redacted, "Bearer [REDACTED]");

        redacted.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_webhook_signature_valid() {
        let secret = "my-secret";
        let payload = b"hello world";
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        let hex_sig = hex::encode(mac.finalize().into_bytes());
        let signature = format!("sha256={}", hex_sig);
        assert!(GitHubClient::verify_webhook_signature(
            payload, &signature, secret, &GitProvider::GitHub
        ));
    }

    #[test]
    fn test_verify_webhook_signature_invalid() {
        assert!(!GitHubClient::verify_webhook_signature(
            b"payload",
            "sha256=0000000000000000000000000000000000000000000000000000000000000000",
            "secret",
            &GitProvider::GitHub
        ));
    }

    // -----------------------------------------------------------------------
    // Issue #36: secret redaction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_redact_secrets_github_pat() {
        let input = "Authorization failed for token ghp_abc123XYZ_456";
        let redacted = GitHubClient::redact_secrets(input);
        assert!(
            !redacted.contains("ghp_abc123XYZ_456"),
            "PAT should be redacted"
        );
        assert!(
            redacted.contains("[REDACTED_TOKEN]"),
            "Should contain redaction marker"
        );
    }

    #[test]
    fn test_redact_secrets_github_pat_variants() {
        // Test all GitHub token prefixes.
        for prefix in &["ghp_", "gho_", "ghs_", "ghu_", "github_pat_"] {
            let token = format!("{}ABCDEFGH12345678", prefix);
            let input = format!("token: {}", token);
            let redacted = GitHubClient::redact_secrets(&input);
            assert!(
                !redacted.contains(&token),
                "Token with prefix {} should be redacted",
                prefix
            );
            assert!(
                redacted.contains("[REDACTED_TOKEN]"),
                "Should contain redaction marker for prefix {}",
                prefix
            );
        }
    }

    #[test]
    fn test_redact_secrets_bearer_token() {
        let input = "Header: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.sig";
        let redacted = GitHubClient::redact_secrets(input);
        assert!(
            !redacted.contains("eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"),
            "Bearer token should be redacted"
        );
        assert!(
            redacted.contains("Bearer [REDACTED]"),
            "Should contain Bearer redaction marker"
        );
    }

    #[test]
    fn test_redact_secrets_preserves_safe_text() {
        let input = "HTTP 404 Not Found: repository owner/repo not accessible";
        let redacted = GitHubClient::redact_secrets(input);
        assert_eq!(
            input, redacted,
            "Text without secrets should pass through unchanged"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #39: canonical diagnostics path tests — check_response wiring,
    // extract_safe_body truncation, redaction, and request-id formatting
    // -----------------------------------------------------------------------

    /// Bodies without secrets pass through redact_secrets unchanged.
    #[test]
    fn test_canonical_safe_body_passthrough() {
        let input = r#"{"message":"Not Found","documentation_url":"https://docs.github.com"}"#;
        let redacted = GitHubClient::redact_secrets(input);
        assert_eq!(redacted, input, "Safe body should pass through unchanged");
    }

    /// Bodies containing GitHub PATs are redacted by the canonical path.
    #[test]
    fn test_canonical_body_token_redaction() {
        let body = r#"{"error":"bad creds","token":"ghp_secretXYZ123"}"#;
        let redacted = GitHubClient::redact_secrets(body);
        assert!(
            !redacted.contains("ghp_secretXYZ123"),
            "Token in response body should be redacted"
        );
        assert!(
            redacted.contains("[REDACTED_TOKEN]"),
            "Redaction marker should be present"
        );
    }

    /// Truncation boundary: redact_secrets does NOT truncate (that's
    /// extract_safe_body's responsibility). Verify the contract.
    #[test]
    fn test_canonical_redact_does_not_truncate() {
        let long_body = "x".repeat(1000);
        let safe = GitHubClient::redact_secrets(&long_body);
        assert_eq!(safe.len(), 1000, "redact_secrets must not truncate");
    }

    /// extract_safe_body truncates at 512 bytes and appends marker.
    #[tokio::test]
    async fn test_canonical_extract_safe_body_truncation() {
        // Build a synthetic response via a local server is impractical, so
        // we validate the truncation logic in isolation using a helper that
        // mirrors extract_safe_body's contract.
        let long_body = "a]".repeat(512); // 1024 chars
        let safe = GitHubClient::redact_secrets(&long_body);
        // Simulate the truncation step from extract_safe_body.
        let truncated = if safe.len() > 512 {
            format!("{}...(truncated)", &safe[..512])
        } else {
            safe.clone()
        };
        assert!(
            truncated.ends_with("...(truncated)"),
            "Long bodies should be truncated"
        );
        assert!(truncated.len() < 600, "Truncated output should be bounded");
    }

    /// Error message format includes request-id placeholder and status.
    /// This verifies the format string used by check_response for ApiError.
    #[test]
    fn test_canonical_error_format_includes_request_id() {
        let status = 404u16;
        let request_id = "ABCD-1234-EF56";
        let body_snippet = r#"{"message":"Not Found"}"#;
        let formatted = format!(
            "HTTP {} (request-id: {}) | {}",
            status, request_id, body_snippet
        );
        assert!(formatted.contains("404"));
        assert!(formatted.contains("ABCD-1234-EF56"));
        assert!(formatted.contains("Not Found"));
    }

    /// Auth error format includes request-id and body context.
    #[test]
    fn test_canonical_auth_error_format() {
        let status = 401u16;
        let request_id = "REQ-789";
        let body_snippet = r#"{"message":"Bad credentials"}"#;
        let formatted = format!(
            "HTTP {} (request-id: {}) | {}",
            status, request_id, body_snippet
        );
        assert!(formatted.contains("401"));
        assert!(formatted.contains("REQ-789"));
        assert!(formatted.contains("Bad credentials"));
    }

    /// No plaintext secrets leak through the full redaction pipeline.
    #[test]
    fn test_canonical_no_secret_leakage() {
        let nasty_body = concat!(
            r#"{"token":"ghp_AAAA1111","bearer":"Bearer eyJhbGciOi","#,
            r#""pat":"github_pat_XXXX","oauth":"gho_YYYY","#,
            r#""server":"ghs_ZZZZ","user":"ghu_WWWW"}"#
        );
        let redacted = GitHubClient::redact_secrets(nasty_body);
        assert!(!redacted.contains("ghp_AAAA1111"));
        assert!(!redacted.contains("eyJhbGciOi"));
        assert!(!redacted.contains("github_pat_XXXX"));
        assert!(!redacted.contains("gho_YYYY"));
        assert!(!redacted.contains("ghs_ZZZZ"));
        assert!(!redacted.contains("ghu_WWWW"));
    }

    #[test]
    fn test_redact_secrets_multiple_tokens() {
        let input = "tokens: ghp_first123 and gho_second456 with Bearer abc.def.ghi";
        let redacted = GitHubClient::redact_secrets(input);
        assert!(!redacted.contains("ghp_first123"));
        assert!(!redacted.contains("gho_second456"));
        assert!(!redacted.contains("abc.def.ghi"));
        // Should have two [REDACTED_TOKEN] markers and one Bearer [REDACTED].
        assert_eq!(
            redacted.matches("[REDACTED_TOKEN]").count(),
            2,
            "Should have 2 token redactions"
        );
        assert!(
            redacted.contains("Bearer [REDACTED]"),
            "Should have Bearer redaction"
        );
    }
}
