//! The git repository a directory belongs to.

use std::path::{Path, PathBuf};

/// The nearest ancestor of `cwd` (itself included) that holds a `.git`
/// directory or file, so worktrees and submodules count.
///
/// Works on the path as given: no canonicalisation and no cache, because the
/// shell's logical `$PWD` is what the hook and the picker both see. `None`
/// for a relative path, which no shell reports.
pub(super) fn find_repo_root(cwd: &Path) -> Option<PathBuf> {
    if !cwd.is_absolute() {
        return None;
    }
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_directory_with_a_git_entry_is_the_root() {
        let root = std::env::temp_dir().join(format!("psychic-cmd-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("r");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("a/b")).unwrap();
        let worktree = root.join("r2");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();

        assert_eq!(find_repo_root(&repo.join("a/b")), Some(repo.clone()));
        assert_eq!(find_repo_root(&repo), Some(repo.clone()));
        assert_eq!(find_repo_root(&root), None);
        assert_eq!(find_repo_root(Path::new("relative/x")), None);
        assert_eq!(find_repo_root(&worktree), Some(worktree.clone()));

        std::fs::remove_dir_all(&root).ok();
    }
}
