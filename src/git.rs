//! Worktrees, branches and status: `git` commands and output parsing.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::exec;

/// A worktree as `git worktree list --porcelain` reports it.
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
pub fn worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let repo = repo.to_string_lossy();
    let output = exec::run(
        "git",
        &["-C", &repo, "worktree", "list", "--porcelain"],
        None,
    )?;
    Ok(parse_worktrees(&output.stdout))
}

/// The main worktree of the repository `dir` is in, whether `dir` is its root, a
/// folder inside it or a linked worktree: git lists the main worktree first.
pub fn main_worktree(dir: &Path) -> Result<PathBuf> {
    match worktrees(dir)?.into_iter().next() {
        Some(main) => Ok(main.path),
        None => Err(Error::NoWorktree {
            path: dir.to_path_buf(),
        }),
    }
}

/// Where a new worktree goes: `<worktree_dir>/<name>` when the project sets
/// `worktree_dir`, else `<repo>.worktrees/<name>` next to the repository. A `/` in
/// the branch name becomes `-`, so every worktree is a direct child of the folder.
pub fn worktree_path(repo: &Path, name: &str, worktree_dir: Option<&Path>) -> PathBuf {
    let folder = name.replace('/', "-");
    match worktree_dir {
        Some(dir) => dir.join(folder),
        None => {
            let mut dir = repo.as_os_str().to_owned();
            dir.push(".worktrees");
            PathBuf::from(dir).join(folder)
        }
    }
}

/// Creates the worktree `path` of `repo` on a new branch `name`, starting from
/// `base`, or from the branch checked out in the main worktree. The name is checked
/// first; git's message comes back as is when it refuses, e.g. a branch that exists.
pub fn add(repo: &Path, name: &str, base: Option<&str>, path: &Path) -> Result<()> {
    check_branch_name(repo, name)?;
    let (repo, path) = (repo.to_string_lossy(), path.to_string_lossy());
    // `--` ends the options, so a base starting with `-` is never read as one
    let mut args = vec!["-C", &repo, "worktree", "add", "-b", name, "--", &path];
    args.extend(base);
    exec::run("git", &args, None)?;
    Ok(())
}

/// A valid branch name is one `git check-ref-format --branch` prints back as is:
/// it also expands `@{-1}` to the branch checked out before, which is not a name.
fn check_branch_name(repo: &Path, name: &str) -> Result<()> {
    let repo = repo.to_string_lossy();
    let invalid = || Error::InvalidBranch {
        name: name.to_string(),
    };
    let args = ["-C", &repo, "check-ref-format", "--branch", name];
    match exec::run("git", &args, None) {
        Ok(output) if output.stdout.trim_end_matches('\n') == name => Ok(()),
        Ok(_) => Err(invalid()),
        // Exit 128 is git's no; anything else, e.g. a missing repository, says why
        Err(Error::Command { status, .. }) if status.code() == Some(128) => Err(invalid()),
        Err(err) => Err(err),
    }
}

/// Removes the linked worktree `path` of `repo`; git refuses one with changes
/// unless `force`, and its message comes back as is. The main worktree is never
/// removed, whatever `force` says.
// Not called yet: removing a worktree uses it (#24)
#[allow(dead_code)]
pub fn remove(repo: &Path, path: &Path, force: bool) -> Result<()> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical(path) == canonical(&main_worktree(repo)?) {
        return Err(Error::MainWorktree {
            path: path.to_path_buf(),
        });
    }
    let (repo, path) = (repo.to_string_lossy(), path.to_string_lossy());
    let mut args = vec!["-C", &repo, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.extend(["--", &path]);
    exec::run("git", &args, None)?;
    Ok(())
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
    fn the_main_worktree_from_anywhere_in_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let repo = dir.join("app");
        fs::create_dir_all(repo.join("src/ui")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["worktree", "add", "-q", "-b", "fix", "../fix"]);
        for inside in [repo.clone(), repo.join("src/ui"), dir.join("fix")] {
            assert_eq!(main_worktree(&inside).unwrap(), repo, "{inside:?}");
        }
    }

    /// A repository at `<dir>/app` with one commit, and `dir`, canonical.
    fn repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().canonicalize().unwrap();
        let repo = dir.join("app");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        (tmp, dir, repo)
    }

    fn head(dir: &Path) -> String {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn worktrees_go_next_to_the_repository_or_in_worktree_dir() {
        let repo = Path::new("/code/app");
        assert_eq!(
            worktree_path(repo, "fix-login", None),
            Path::new("/code/app.worktrees/fix-login")
        );
        assert_eq!(
            worktree_path(repo, "feature/login", Some(Path::new("/wt"))),
            Path::new("/wt/feature-login")
        );
    }

    #[test]
    fn adds_a_worktree_on_a_new_branch() {
        let (_tmp, dir, repo) = repository();
        let path = worktree_path(&repo, "feature/login", None);
        add(&repo, "feature/login", None, &path).unwrap();
        let worktrees = worktrees(&repo).unwrap();
        assert_eq!(worktrees[1].path, dir.join("app.worktrees/feature-login"));
        assert_eq!(worktrees[1].branch.as_deref(), Some("feature/login"));
        assert_eq!(head(&path), head(&repo), "from the main worktree's branch");

        // In a folder of its own, from a base
        git(&repo, &["branch", "base"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "later"]);
        let custom = dir.join("elsewhere");
        let path = worktree_path(&repo, "fix", Some(&custom));
        add(&repo, "fix", Some("base"), &path).unwrap();
        assert_eq!(path, custom.join("fix"));
        assert_ne!(head(&path), head(&repo), "from base, not main");
    }

    #[test]
    fn invalid_names_are_rejected_before_anything_runs() {
        let (_tmp, dir, repo) = repository();
        // `@{-1}` is the branch checked out before: valid to git, but not a name
        git(&repo, &["checkout", "-q", "-b", "other"]);
        git(&repo, &["checkout", "-q", "main"]);
        for name in ["a..b", "-x", "with space", "", "HEAD", "@{-1}", "x.lock"] {
            let path = dir.join("wt");
            let err = add(&repo, name, None, &path).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidBranch { name: n } if n == name),
                "{name:?}: {err:?}"
            );
            assert!(!path.exists(), "{name:?}");
        }
        assert_eq!(worktrees(&repo).unwrap().len(), 1);
    }

    #[test]
    fn git_says_why_it_cannot_add() {
        let (_tmp, dir, repo) = repository();
        let err = add(&repo, "main", None, &dir.join("wt")).unwrap_err();
        let Error::Command { stderr, .. } = err else {
            panic!("{err:?}");
        };
        assert!(stderr.contains("already exists"), "{stderr}");
        // A base is a commit, never an option
        let err = add(&repo, "x", Some("--detach"), &dir.join("wt")).unwrap_err();
        let Error::Command { stderr, .. } = err else {
            panic!("{err:?}");
        };
        assert!(stderr.contains("invalid reference: --detach"), "{stderr}");
    }

    #[test]
    fn removes_a_clean_worktree_and_a_dirty_one_only_when_forced() {
        let (_tmp, _dir, repo) = repository();
        let clean = worktree_path(&repo, "clean", None);
        let dirty = worktree_path(&repo, "dirty", None);
        add(&repo, "clean", None, &clean).unwrap();
        add(&repo, "dirty", None, &dirty).unwrap();
        fs::write(dirty.join("notes.md"), "draft").unwrap();

        remove(&repo, &clean, false).unwrap();
        assert!(!clean.exists());

        let err = remove(&repo, &dirty, false).unwrap_err();
        let Error::Command { stderr, .. } = err else {
            panic!("{err:?}");
        };
        assert!(
            stderr.contains("contains modified or untracked files"),
            "{stderr}"
        );
        assert!(dirty.join("notes.md").exists(), "left as it was");

        remove(&repo, &dirty, true).unwrap();
        assert!(!dirty.exists());
        assert_eq!(worktrees(&repo).unwrap().len(), 1);
    }

    #[test]
    fn the_main_worktree_is_never_removed() {
        let (_tmp, dir, repo) = repository();
        let linked = worktree_path(&repo, "fix", None);
        add(&repo, "fix", None, &linked).unwrap();
        // However it is named, and from a linked worktree too
        let through_link = dir.join("link");
        std::os::unix::fs::symlink(&repo, &through_link).unwrap();
        for (from, main) in [(&repo, &repo), (&linked, &through_link)] {
            for force in [false, true] {
                let err = remove(from, main, force).unwrap_err();
                assert!(matches!(err, Error::MainWorktree { .. }), "{err:?}");
            }
        }
        assert!(repo.join(".git").exists());
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
