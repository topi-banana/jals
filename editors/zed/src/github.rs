//! Querying the GitHub Actions API for the newest `jals` binary artifact. Only the artifact
//! *listing* is done here (anonymously); the download itself — which GitHub gates behind
//! authentication even for public repositories — is handled through nightly.link in [`crate::cache`].

use zed_extension_api::{
    http_client::{HttpMethod, HttpRequest, RedirectPolicy},
    serde_json, Result,
};

use crate::GITHUB_REPO;

/// The GitHub Actions artifacts API for the [`GITHUB_REPO`] repository.
pub(crate) struct Github;

impl Github {
    /// Looks up the id of the newest un-expired `main`-branch artifact with the given name.
    /// The listing endpoint — unlike the artifact download — allows anonymous access.
    pub(crate) fn latest_artifact_id(artifact_name: &str) -> Result<u64> {
        let url = format!(
            "https://api.github.com/repos/{GITHUB_REPO}/actions/artifacts?name={artifact_name}&per_page=20"
        );
        let request = HttpRequest::builder()
            .method(HttpMethod::Get)
            .url(&url)
            // The GitHub API rejects requests carrying no User-Agent.
            .header("User-Agent", "zed-jals-extension")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .redirect_policy(RedirectPolicy::FollowLimit(5))
            .build()?;
        let response = request
            .fetch()
            .map_err(|err| format!("fetching `{url}` failed: {err}"))?;
        let body: serde_json::Value = serde_json::from_slice(&response.body)
            .map_err(|err| format!("`{url}` returned invalid JSON: {err}"))?;

        body.get("artifacts")
            .and_then(|artifacts| artifacts.as_array())
            .ok_or_else(|| format!("`{url}` returned no artifact list"))?
            .iter()
            // Artifacts are listed newest-first; take the first live one built on `main`.
            .find_map(|artifact| {
                let expired = artifact
                    .get("expired")
                    .and_then(|expired| expired.as_bool())
                    .unwrap_or(true);
                let branch = artifact
                    .pointer("/workflow_run/head_branch")
                    .and_then(|branch| branch.as_str());
                if expired || branch != Some("main") {
                    return None;
                }
                artifact.get("id").and_then(|id| id.as_u64())
            })
            .ok_or_else(|| {
                format!("no un-expired `{artifact_name}` artifact from `main` was found")
            })
    }
}
