//! Pull requests: `gh` commands and output parsing.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::exec;

/// The fields `gh pr list` is asked for, as `--json` takes them.
const FIELDS: &str = "number,headRefName,state,isDraft,isCrossRepository,statusCheckRollup";

/// A branch's pull request, for its badge.
#[derive(Debug, Clone, PartialEq)]
pub struct Pr {
    pub number: u32,
    pub state: State,
    /// `None` when it has no checks.
    pub checks: Option<Checks>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

/// Every check of a pull request, summed up as GitHub shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    /// `✓`: all passed, skipped or neutral.
    Passed,
    /// `✗`: any failed.
    Failed,
    /// `●`: none failed, and any still running or waiting.
    Pending,
}

/// The pull requests of the GitHub repository of `project`, by the branch they
/// come from, as `gh` sees them from that folder. Fails with gh's own message
/// when it is missing, not logged in, or the project has no GitHub remote.
pub fn prs(project: &Path) -> Result<HashMap<String, Pr>> {
    let args = [
        "pr", "list", "--state", "all", "--limit", "100", "--json", FIELDS,
    ];
    let output = exec::run("gh", &args, Some(project))?;
    parse_prs(&output.stdout)
}

/// Opens pull request `number` of `project`'s GitHub repository in the browser.
pub fn open(project: &Path, number: u32) -> Result<()> {
    let number = number.to_string();
    exec::run("gh", &["pr", "view", "--web", &number], Some(project))?;
    Ok(())
}

/// One entry of `gh pr list --json`; other fields are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    number: u32,
    head_ref_name: String,
    state: String,
    is_draft: bool,
    is_cross_repository: bool,
    // `null` for a pull request whose commit has no checks
    #[serde(default)]
    status_check_rollup: Option<Vec<Rollup>>,
}

/// A check of the rollup: a check run (GitHub Actions, apps), or a commit
/// status (older CI services).
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Rollup {
    CheckRun {
        status: String,
        /// Empty until it has completed.
        #[serde(default)]
        conclusion: String,
    },
    StatusContext {
        state: String,
    },
    #[serde(other)]
    Other,
}

/// Parses `gh pr list --json` into a pull request per branch. Pull requests from
/// forks are left out: their branch is not one of the project's, whatever its
/// name. When a branch has several, an open one wins, then the newest.
fn parse_prs(text: &str) -> Result<HashMap<String, Pr>> {
    let entries: Vec<Entry> =
        serde_json::from_str(text).map_err(|err| Error::PrList(err.to_string()))?;
    let mut prs: HashMap<String, Pr> = HashMap::new();
    for entry in entries {
        if entry.is_cross_repository {
            continue;
        }
        let state = match (entry.state.as_str(), entry.is_draft) {
            ("OPEN", true) => State::Draft,
            ("OPEN", false) => State::Open,
            ("MERGED", _) => State::Merged,
            ("CLOSED", _) => State::Closed,
            // A state gh may add later: no badge rather than a wrong one
            _ => continue,
        };
        let pr = Pr {
            number: entry.number,
            state,
            checks: summary(entry.status_check_rollup.as_deref().unwrap_or_default()),
        };
        let open = |pr: &Pr| matches!(pr.state, State::Open | State::Draft);
        match prs.get(&entry.head_ref_name) {
            Some(kept) if (open(kept), kept.number) > (open(&pr), pr.number) => {}
            _ => {
                prs.insert(entry.head_ref_name, pr);
            }
        }
    }
    Ok(prs)
}

/// Any failed check makes it failed, else any running or waiting pending, else
/// any passed passed. Results GitHub may add later count as none of them.
fn summary(checks: &[Rollup]) -> Option<Checks> {
    let mut summary = None;
    for check in checks {
        let result = match check {
            Rollup::CheckRun { status, .. } if status != "COMPLETED" => Some(Checks::Pending),
            Rollup::CheckRun { conclusion, .. } => match conclusion.as_str() {
                "SUCCESS" | "NEUTRAL" | "SKIPPED" => Some(Checks::Passed),
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                | "STARTUP_FAILURE" => Some(Checks::Failed),
                _ => None,
            },
            Rollup::StatusContext { state } => match state.as_str() {
                "SUCCESS" => Some(Checks::Passed),
                "FAILURE" | "ERROR" => Some(Checks::Failed),
                "PENDING" | "EXPECTED" => Some(Checks::Pending),
                _ => None,
            },
            Rollup::Other => None,
        };
        summary = match (summary, result) {
            (Some(Checks::Failed), _) | (_, Some(Checks::Failed)) => Some(Checks::Failed),
            (Some(Checks::Pending), _) | (_, Some(Checks::Pending)) => Some(Checks::Pending),
            (Some(Checks::Passed), _) | (_, Some(Checks::Passed)) => Some(Checks::Passed),
            (None, None) => None,
        };
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> HashMap<String, Pr> {
        let text = match name {
            "none" => include_str!("../tests/fixtures/gh/prs-none.json"),
            "checks" => include_str!("../tests/fixtures/gh/prs-checks.json"),
            "states" => include_str!("../tests/fixtures/gh/prs-states.json"),
            "one-branch" => include_str!("../tests/fixtures/gh/prs-one-branch.json"),
            _ => unreachable!("no fixture {name}"),
        };
        parse_prs(text).unwrap()
    }

    /// Each branch's number and checks, sorted by branch.
    fn checks(prs: &HashMap<String, Pr>) -> Vec<(&str, u32, Option<Checks>)> {
        let mut list: Vec<_> = (prs.iter())
            .map(|(branch, pr)| (branch.as_str(), pr.number, pr.checks))
            .collect();
        list.sort_by_key(|(branch, ..)| *branch);
        list
    }

    #[test]
    fn no_pull_requests() {
        assert_eq!(fixture("none"), HashMap::new());
    }

    #[test]
    fn checks_are_summed_up_as_github_shows_them() {
        let prs = fixture("checks");
        assert_eq!(
            checks(&prs),
            [
                ("cancelled", 43, Some(Checks::Failed)),
                ("failing", 47, Some(Checks::Failed)),
                ("no-checks", 42, None),
                ("passing", 48, Some(Checks::Passed)),
                ("pending", 46, Some(Checks::Pending)),
                ("status-error", 44, Some(Checks::Failed)),
                ("status-pending", 45, Some(Checks::Pending)),
            ]
        );
        assert_eq!(prs["passing"].state, State::Open);
    }

    #[test]
    fn open_draft_merged_and_closed() {
        let prs = fixture("states");
        let states = |branch: &str| prs[branch].state;
        assert_eq!(states("wip"), State::Draft);
        assert_eq!(states("shipped"), State::Merged);
        assert_eq!(states("abandoned"), State::Closed);
        assert_eq!(prs["abandoned"].checks, Some(Checks::Failed));
    }

    #[test]
    fn an_open_pull_request_wins_then_the_newest_and_forks_never() {
        let prs = fixture("one-branch");
        assert_eq!(
            checks(&prs),
            [("fix", 61, Some(Checks::Passed)), ("retry", 62, None),],
            "the open #61 over the newer closed #63; the closed #62 over the merged #60; \
             the fork's main left out"
        );
    }

    #[test]
    fn null_rollups_and_unknown_kinds_are_no_checks() {
        let text = r#"[
            {"number": 1, "headRefName": "a", "state": "OPEN", "isDraft": false,
             "isCrossRepository": false, "statusCheckRollup": null, "url": "u"},
            {"number": 2, "headRefName": "b", "state": "OPEN", "isDraft": false,
             "isCrossRepository": false, "url": "u",
             "statusCheckRollup": [{"__typename": "SomethingNew", "state": "FAILURE"}]},
            {"number": 3, "headRefName": "c", "state": "LOCKED", "isDraft": false,
             "isCrossRepository": false, "statusCheckRollup": [], "url": "u"}
        ]"#;
        let prs = parse_prs(text).unwrap();
        assert_eq!(checks(&prs), [("a", 1, None), ("b", 2, None)]);
    }

    #[test]
    fn output_that_is_not_the_list_is_an_error() {
        let err = parse_prs("no pull requests match your search").unwrap_err();
        assert!(matches!(err, Error::PrList(_)), "{err}");
    }
}
