//! GitHub access for autonomous Forge runs, behind a trait so the runner can be tested
//! without network access. The production implementation shells out to the `gh` CLI
//! with bounded timeouts; it reads issues/PRs and opens draft PRs, and never merges.

use serde::Deserialize;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::admission::IssueCandidate;

/// An open pull request, used to avoid duplicate work on an issue.
#[derive(Debug, Clone, Default)]
pub struct OpenPullRequest {
    pub number: u64,
    pub head_branch: String,
    pub body: String,
}

/// A draft pull request to open from an already-pushed branch.
#[derive(Debug, Clone)]
pub struct DraftPr {
    pub head_branch: String,
    pub base_branch: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

pub trait GithubClient: Send + Sync {
    fn list_open_issues(&self, repo: &str, label: &str) -> Result<Vec<IssueCandidate>, String>;
    fn open_pull_requests(&self, repo: &str) -> Result<Vec<OpenPullRequest>, String>;
    /// Open a draft PR and return its URL.
    fn create_draft_pr(&self, repo: &str, pr: &DraftPr) -> Result<String, String>;
}

/// Set `has_open_pr` on issues that an open PR already works on, either through the
/// Forge branch name or a `#N` reference in the PR body.
pub fn mark_open_prs(issues: &mut [IssueCandidate], prs: &[OpenPullRequest]) {
    for issue in issues.iter_mut() {
        let branch = format!("agent/forge-issue-{}", issue.number);
        let reference = format!("#{}", issue.number);
        issue.has_open_pr = prs.iter().any(|pr| {
            pr.head_branch == branch
                || pr.body.match_indices(&reference).any(|(i, _)| {
                    !pr.body[i + reference.len()..].starts_with(|c: char| c.is_ascii_digit())
                })
        });
    }
}

/// Run a command with a wall-clock timeout, returning stdout on success.
pub(crate) fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn {:?}: {e}", cmd.get_program()))?;

    // Drain pipes on threads so a chatty child cannot block on a full pipe buffer.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let err_handle = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{:?} timed out after {}s",
                    cmd.get_program(),
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out_handle.join().unwrap_or_default();
    let err = err_handle.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        Err(format!(
            "{:?} exited with {status}: {}",
            cmd.get_program(),
            err.trim()
        ))
    }
}

/// `gh` CLI implementation using the host's authenticated account.
pub struct GhCliClient {
    bin: String,
    timeout: Duration,
}

impl GhCliClient {
    pub fn new() -> Self {
        Self {
            bin: std::env::var("SAIL_GH_BIN").unwrap_or_else(|_| "gh".into()),
            timeout: Duration::from_secs(60),
        }
    }

    fn gh(&self, args: &[&str]) -> Result<String, String> {
        run_with_timeout(Command::new(&self.bin).args(args), self.timeout)
    }
}

impl Default for GhCliClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(Deserialize)]
struct GhIssue {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    head_ref_name: String,
    #[serde(default)]
    body: String,
}

impl GithubClient for GhCliClient {
    fn list_open_issues(&self, repo: &str, label: &str) -> Result<Vec<IssueCandidate>, String> {
        let out = self.gh(&[
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--label",
            label,
            "--limit",
            "100",
            "--json",
            "number,title,body,labels",
        ])?;
        let issues: Vec<GhIssue> =
            serde_json::from_str(&out).map_err(|e| format!("invalid gh issue JSON: {e}"))?;
        Ok(issues
            .into_iter()
            .map(|i| IssueCandidate {
                number: i.number,
                title: i.title,
                body: i.body,
                labels: i.labels.into_iter().map(|l| l.name).collect(),
                has_open_pr: false,
            })
            .collect())
    }

    fn open_pull_requests(&self, repo: &str) -> Result<Vec<OpenPullRequest>, String> {
        let out = self.gh(&[
            "pr",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            "200",
            "--json",
            "number,headRefName,body",
        ])?;
        let prs: Vec<GhPr> =
            serde_json::from_str(&out).map_err(|e| format!("invalid gh pr JSON: {e}"))?;
        Ok(prs
            .into_iter()
            .map(|p| OpenPullRequest {
                number: p.number,
                head_branch: p.head_ref_name,
                body: p.body,
            })
            .collect())
    }

    fn create_draft_pr(&self, repo: &str, pr: &DraftPr) -> Result<String, String> {
        let args = vec![
            "pr",
            "create",
            "--repo",
            repo,
            "--draft",
            "--base",
            &pr.base_branch,
            "--head",
            &pr.head_branch,
            "--title",
            &pr.title,
            "--body",
            &pr.body,
        ];
        let with_labels: Vec<&str> = pr
            .labels
            .iter()
            .flat_map(|l| ["--label", l.as_str()])
            .collect();
        let mut labelled = args.clone();
        labelled.extend(with_labels);
        match self.gh(&labelled) {
            Ok(out) => Ok(out.trim().to_string()),
            // A missing label must not lose the PR: retry without labels.
            Err(e) if !pr.labels.is_empty() && e.contains("label") => {
                self.gh(&args).map(|out| out.trim().to_string())
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pr_detection_by_branch_and_reference() {
        let mut issues = vec![
            IssueCandidate {
                number: 12,
                ..Default::default()
            },
            IssueCandidate {
                number: 13,
                ..Default::default()
            },
            IssueCandidate {
                number: 1,
                ..Default::default()
            },
        ];
        let prs = vec![
            OpenPullRequest {
                number: 200,
                head_branch: "agent/forge-issue-12".into(),
                body: String::new(),
            },
            OpenPullRequest {
                number: 201,
                head_branch: "agent/other".into(),
                body: "Closes #13 and refs #150".into(),
            },
        ];
        mark_open_prs(&mut issues, &prs);
        assert!(issues[0].has_open_pr);
        assert!(issues[1].has_open_pr);
        // `#150` must not count as a reference to #1.
        assert!(!issues[2].has_open_pr);
    }

    #[test]
    fn timeout_kills_slow_command() {
        let err = run_with_timeout(Command::new("sleep").arg("5"), Duration::from_millis(200))
            .unwrap_err();
        assert!(err.contains("timed out"));
    }

    #[test]
    fn failing_command_reports_stderr() {
        let err = run_with_timeout(
            Command::new("sh").args(["-c", "echo boom >&2; exit 3"]),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(err.contains("boom"));
    }
}
