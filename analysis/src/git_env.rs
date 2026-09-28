//! `git` subprocesses that act on the repository they are pointed at.
//!
//! Git exports `GIT_DIR`, `GIT_INDEX_FILE`, `GIT_WORK_TREE`, … to the hooks
//! it runs. A `git` started from a hook inherits them and acts on the
//! hook's repository and index, not on its `current_dir` or `-C` path:
//! codegraph runs from hooks (`codegraph sync --quiet` in post-commit), and
//! a test suite run by a pre-push hook once flipped the real repository's
//! `core.bare` and staged its fixtures into the real index. Every `git`
//! codegraph (or its tests) spawns goes through [`git`], which drops git's
//! own list of repository-local variables (`git rev-parse --local-env-vars`,
//! what git clears for submodules itself).

use std::process::Command;

/// `git rev-parse --local-env-vars` (git 2.54).
pub const LOCAL_ENV_VARS: &[&str] = &[
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// A `git` command that ignores any repository a parent git process chose
/// for it: set `current_dir` or `-C` to say which repository.
pub fn git() -> Command {
    let mut command = Command::new("git");
    for var in LOCAL_ENV_VARS {
        command.env_remove(var);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_repository_does_not_leak_into_git() {
        let command = git();
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        for var in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "GIT_COMMON_DIR",
        ] {
            assert!(removed.iter().any(|r| r == var), "{var} not removed");
        }
    }

    /// The list matches the installed git's own, when git is there.
    #[test]
    fn the_list_is_gits_own() {
        let Ok(out) = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
        else {
            return;
        };
        if !out.status.success() {
            return;
        }
        for var in String::from_utf8_lossy(&out.stdout).split_whitespace() {
            assert!(LOCAL_ENV_VARS.contains(&var), "git lists {var}; add it");
        }
    }
}
