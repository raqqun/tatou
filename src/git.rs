//! Worktrees, branches and status: `git` commands and output parsing.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::exec;

/// A worktree as `git worktree list --porcelain` reports it.
// Not read yet beyond the model, itself unused until (#13)
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
pub struct Worktree {
    /// Absolute, with symlinks resolved by git.
    pub path: PathBuf,
    /// The commit checked out; `None` for a bare repository. All zeros before the first commit.
    pub head: Option<String>,
    /// The branch checked out, without `refs/heads/`; `None` when detached or bare.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    /// Set when locked, with the reason given to `git worktree lock`, or empty.
    pub locked: Option<String>,
    /// Set when `git worktree prune` would remove it, with git's reason (its directory is gone).
    pub prunable: Option<String>,
}

/// Lists the worktrees of the repository at `repo`; the first is the main worktree.
/// Fails with git's own message when `repo` is not a repository, or when git is missing.
// Not called yet: the background refresh lists each project's worktrees (#13)
#[allow(dead_code)]
pub fn worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let repo = repo.to_string_lossy();
    let output = exec::run(
        "git",
        &["-C", &repo, "worktree", "list", "--porcelain"],
        None,
    )?;
    Ok(parse_worktrees(&output.stdout))
}

/// Parses `git worktree list --porcelain`: one block of lines per worktree, each
/// block starting with `worktree <path>` and ending with an empty line. Lines git
/// may add in later versions are ignored. Paths are printed as is, so one with a
/// newline in it cannot be read back; tatou does not support such paths.
fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            worktrees.extend(current.take());
            current = Some(Worktree {
                path: PathBuf::from(value),
                head: None,
                branch: None,
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
            });
            continue;
        }
        let Some(worktree) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => worktree.head = Some(value.to_string()),
            "branch" => {
                let branch = value.strip_prefix("refs/heads/").unwrap_or(value);
                worktree.branch = Some(branch.to_string());
            }
            "detached" => worktree.detached = true,
            "bare" => worktree.bare = true,
            "locked" => worktree.locked = Some(value.to_string()),
            "prunable" => worktree.prunable = Some(value.to_string()),
            "" => worktrees.extend(current.take()),
            _ => {}
        }
    }
    worktrees.extend(current);
    worktrees
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;
    use crate::error::Error;

    const HEAD: &str = "5fd71d0a678bf10d8078991b7286f696fe659c1f";

    fn on_branch(path: &str, branch: &str) -> Worktree {
        Worktree {
            path: path.into(),
            head: Some(HEAD.into()),
            branch: Some(branch.into()),
            detached: false,
            bare: false,
            locked: None,
            prunable: None,
        }
    }

    #[test]
    fn parses_a_single_worktree() {
        let text = include_str!("../tests/fixtures/git/worktrees-single.txt");
        assert_eq!(
            parse_worktrees(text),
            [on_branch("/home/me/code/my-app", "main")]
        );
    }

    #[test]
    fn parses_detached_locked_and_prunable_worktrees() {
        let text = include_str!("../tests/fixtures/git/worktrees-several.txt");
        let dir = "/home/me/code/my-app.worktrees";
        let expected = [
            on_branch("/home/me/code/my-app", "main"),
            Worktree {
                branch: None,
                detached: true,
                ..on_branch(&format!("{dir}/bisect"), "")
            },
            Worktree {
                locked: Some(String::new()),
                ..on_branch(&format!("{dir}/docs"), "docs")
            },
            on_branch(&format!("{dir}/fix login"), "fix-login"),
            Worktree {
                prunable: Some("gitdir file points to non-existent location".into()),
                ..on_branch(&format!("{dir}/old"), "old")
            },
            Worktree {
                locked: Some("on a USB drive".into()),
                ..on_branch(&format!("{dir}/spike"), "spike")
            },
        ];
        assert_eq!(parse_worktrees(text), expected);
    }

    #[test]
    fn parses_a_bare_main_repository() {
        let text = include_str!("../tests/fixtures/git/worktrees-bare.txt");
        let bare = Worktree {
            head: None,
            branch: None,
            bare: true,
            ..on_branch("/home/me/code/api.git", "")
        };
        let expected = [
            bare,
            on_branch("/home/me/code/api.worktrees/main", "main-wt"),
        ];
        assert_eq!(parse_worktrees(text), expected);
    }

    #[test]
    fn parses_a_repository_without_commits() {
        let text = include_str!("../tests/fixtures/git/worktrees-unborn.txt");
        let unborn = Worktree {
            head: Some("0".repeat(40)),
            ..on_branch("/home/me/code/empty", "main")
        };
        assert_eq!(parse_worktrees(text), [unborn]);
    }

    #[test]
    fn ignores_unknown_lines_and_text_before_the_first_worktree() {
        let text = "stray\n\nworktree /a\nHEAD 1\nbranch refs/heads/x\nnew-attribute y\n";
        let expected = Worktree {
            head: Some("1".into()),
            ..on_branch("/a", "x")
        };
        assert_eq!(parse_worktrees(text), [expected]);
        assert_eq!(parse_worktrees(""), []);
    }

    /// Runs git in `dir` without the user's or the system's config, so that their
    /// hooks or commit signing never get in the way.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "tatou")
            .env("GIT_AUTHOR_EMAIL", "tatou@example.com")
            .env("GIT_COMMITTER_NAME", "tatou")
            .env("GIT_COMMITTER_EMAIL", "tatou@example.com")
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn lists_the_worktrees_of_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        // git prints paths with symlinks resolved: /var is /private/var on macOS
        let dir = dir.path().canonicalize().unwrap();
        let repo = dir.join("my app");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "fix", "../fix login"],
        );

        let worktrees = worktrees(&repo).unwrap();
        let summary: Vec<_> = worktrees
            .iter()
            .map(|w| (w.path.clone(), w.branch.as_deref(), w.head.is_some()))
            .collect();
        let expected = [
            (repo.clone(), Some("main"), true),
            (dir.join("fix login"), Some("fix"), true),
        ];
        assert_eq!(summary, expected);
        assert_eq!(worktrees[0].head, worktrees[1].head);
    }

    #[test]
    fn a_directory_outside_a_repository_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = worktrees(dir.path()).unwrap_err();
        let Error::Command { stderr, .. } = err else {
            panic!("unexpected error: {err:?}");
        };
        assert!(stderr.contains("not a git repository"), "{stderr}");
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = worktrees(&dir.path().join("missing")).unwrap_err();
        assert!(matches!(err, Error::Command { .. }), "{err:?}");
    }
}
