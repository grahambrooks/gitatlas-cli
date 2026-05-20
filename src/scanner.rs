use std::path::PathBuf;

use rayon::prelude::*;

use crate::git::{discovery, status};
use crate::models::RepoInfo;

/// Scan multiple root directories for Git repositories.
/// Status collection is parallelized via rayon since each repo is independent.
pub fn scan_roots(roots: &[PathBuf]) -> Vec<RepoInfo> {
    let mut repo_paths: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| discovery::discover_repos(root))
        .collect();
    repo_paths.sort();
    repo_paths.dedup();

    let mut all_repos: Vec<RepoInfo> = repo_paths
        .par_iter()
        .map(|p| status::get_repo_info(p))
        .collect();

    all_repos.sort_by_key(|r| r.name.to_lowercase());
    all_repos
}
