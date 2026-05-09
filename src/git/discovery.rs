use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

/// Discover all Git repositories under the given root directory.
pub fn discover_repos(root: &Path) -> Vec<PathBuf> {
    let mut repos = Vec::new();

    if !root.exists() || !root.is_dir() {
        return repos;
    }

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .max_depth(Some(5))
        .build();

    for entry in walker.flatten() {
        let path = entry.path();
        if path.file_name().map_or(false, |n| n == ".git") && path.is_dir() {
            if let Some(parent) = path.parent() {
                repos.push(parent.to_path_buf());
            }
        }
    }

    repos
}
