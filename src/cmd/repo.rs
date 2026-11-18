//! The git repository a directory belongs to.

use std::path::{Path, PathBuf};

/// The nearest ancestor of `cwd` (itself included) that holds a `.git`
/// directory or file, so worktrees and submodules count.
///
/// The walk stops at `home`: a `.git` in the home directory itself (a
/// dotfiles repository) or above it does not make every directory a
/// repository, so for a `cwd` under `home` only directories strictly below
/// `home` count. A `cwd` outside `home` (or no `home`) is walked to the root.
///
/// Works on the paths as given: no canonicalisation and no cache, because the
/// shell's logical `$PWD` is what the hook and the picker both see. `None`
/// for a relative path, which no shell reports.
pub(super) fn find_repo_root(cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if !cwd.is_absolute() {
        return None;
    }
    // Paths compare by component, so a trailing slash on `home` is no matter.
    let stop = home.filter(|home| cwd.starts_with(home));
    cwd.ancestors()
        .take_while(|dir| Some(*dir) != stop)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch tree that holds its own `home`, so no answer depends on what
    /// lies above the system temp dir (which may itself be under a `$HOME`
    /// with a `.git`).
    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn new(tag: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("psychic-cmd-{}-{}", tag, std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Tree { root }
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let dir = self.root.join(relative);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).ok();
        }
    }

    #[test]
    fn the_nearest_directory_with_a_git_entry_is_the_root() {
        let tree = Tree::new("repo");
        let home = tree.dir("home");
        let repo = tree.dir("home/r");
        tree.dir("home/r/.git");
        let deep = tree.dir("home/r/a/b");
        let worktree = tree.dir("home/r2");
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let plain = tree.dir("home/plain");

        let root = |cwd: &Path| find_repo_root(cwd, Some(&home));
        assert_eq!(root(&deep), Some(repo.clone()));
        assert_eq!(root(&repo), Some(repo.clone()));
        assert_eq!(root(&worktree), Some(worktree.clone()), ".git as a file");
        assert_eq!(root(&plain), None);
        assert_eq!(root(&home), None);
        assert_eq!(root(Path::new("relative/x")), None);
    }

    #[test]
    fn a_git_entry_in_home_itself_is_no_repository() {
        let tree = Tree::new("repo-home");
        let home = tree.dir("home");
        tree.dir("home/.git");
        let plain = tree.dir("home/notes/2024");
        let nested = tree.dir("home/src/app");
        tree.dir("home/src/app/.git");

        assert_eq!(find_repo_root(&plain, Some(&home)), None);
        assert_eq!(find_repo_root(&home, Some(&home)), None);
        assert_eq!(
            find_repo_root(&nested.join("src"), Some(&home)),
            Some(nested.clone()),
            "a repository strictly below home still counts"
        );
        // A trailing slash on HOME names the same directory.
        let slashed = PathBuf::from(format!("{}/", home.display()));
        assert_eq!(find_repo_root(&plain, Some(&slashed)), None);
        // Without a home, the walk goes up and finds the dotfiles repository.
        assert_eq!(find_repo_root(&plain, None), Some(home.clone()));
    }

    #[test]
    fn a_directory_outside_home_is_walked_up_to_its_repository() {
        let tree = Tree::new("repo-outside");
        let home = tree.dir("home");
        tree.dir("home/.git");
        let repo = tree.dir("srv/repo");
        tree.dir("srv/repo/.git");
        let deep = tree.dir("srv/repo/x/y");

        assert_eq!(find_repo_root(&deep, Some(&home)), Some(repo));
    }
}
