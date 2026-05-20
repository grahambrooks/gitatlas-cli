use git2::Repository;
use std::path::Path;

use crate::models::{RepoHealth, RepoInfo};

/// Get the full status of a Git repository.
pub fn get_repo_info(path: &Path) -> RepoInfo {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let now = chrono::Utc::now().to_rfc3339();

    let mut repo = match Repository::open(path) {
        Ok(r) => r,
        Err(_) => {
            return RepoInfo {
                path: path.to_string_lossy().to_string(),
                name,
                branch: "unknown".to_string(),
                ahead: 0,
                behind: 0,
                dirty_files: 0,
                stash_count: 0,
                health: RepoHealth::Error,
                last_checked: now,
                remote_url: None,
            };
        }
    };

    let branch = get_branch_name(&repo);
    let (ahead, behind) = get_ahead_behind(&repo);
    let dirty_files = get_dirty_count(&repo);
    let stash_count = get_stash_count(&mut repo);
    let remote_url = get_origin_url(&repo);

    let health = determine_health(ahead, behind, dirty_files);

    RepoInfo {
        path: path.to_string_lossy().to_string(),
        name,
        branch,
        ahead,
        behind,
        dirty_files,
        stash_count,
        health,
        last_checked: now,
        remote_url,
    }
}

fn get_branch_name(repo: &Repository) -> String {
    repo.head()
        .ok()
        .and_then(|head| head.shorthand().map(String::from))
        .unwrap_or_else(|| "HEAD (detached)".to_string())
}

fn get_ahead_behind(repo: &Repository) -> (u32, u32) {
    let head = match repo.head() {
        Ok(h) => h,
        Err(_) => return (0, 0),
    };

    let local_oid = match head.target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    let branch_name = match head.shorthand() {
        Some(name) => name.to_string(),
        None => return (0, 0),
    };

    let upstream_name = format!("refs/remotes/origin/{}", branch_name);
    let upstream_ref = match repo.find_reference(&upstream_name) {
        Ok(r) => r,
        Err(_) => return (0, 0),
    };

    let upstream_oid = match upstream_ref.target() {
        Some(oid) => oid,
        None => return (0, 0),
    };

    repo.graph_ahead_behind(local_oid, upstream_oid)
        .map(|(a, b)| (a as u32, b as u32))
        .unwrap_or((0, 0))
}

fn get_dirty_count(repo: &Repository) -> u32 {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false);

    repo.statuses(Some(&mut opts))
        .map(|statuses| statuses.len() as u32)
        .unwrap_or(0)
}

fn get_stash_count(repo: &mut Repository) -> u32 {
    let mut count = 0u32;
    let _ = repo.stash_foreach(|_, _, _| {
        count += 1;
        true
    });
    count
}

fn get_origin_url(repo: &Repository) -> Option<String> {
    repo.find_remote("origin")
        .ok()
        .and_then(|r| r.url().map(String::from))
}

fn determine_health(ahead: u32, behind: u32, dirty_files: u32) -> RepoHealth {
    if behind > 0 && (ahead > 0 || dirty_files > 0) {
        RepoHealth::Diverged
    } else if dirty_files > 0 || ahead > 0 {
        RepoHealth::Dirty
    } else {
        RepoHealth::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_clean_when_in_sync() {
        assert_eq!(determine_health(0, 0, 0), RepoHealth::Clean);
    }

    #[test]
    fn health_dirty_with_local_only_changes() {
        // ahead, or dirty files, but not behind => dirty.
        assert_eq!(determine_health(1, 0, 0), RepoHealth::Dirty);
        assert_eq!(determine_health(0, 0, 5), RepoHealth::Dirty);
        assert_eq!(determine_health(2, 0, 3), RepoHealth::Dirty);
    }

    #[test]
    fn health_behind_only_reports_clean() {
        // Quirk: being purely behind upstream (no local commits or dirty files)
        // is currently reported as Clean, not Dirty/Diverged.
        assert_eq!(determine_health(0, 4, 0), RepoHealth::Clean);
    }

    #[test]
    fn health_diverged_when_behind_and_local_changes() {
        assert_eq!(determine_health(1, 1, 0), RepoHealth::Diverged);
        assert_eq!(determine_health(0, 1, 2), RepoHealth::Diverged);
        assert_eq!(determine_health(3, 2, 1), RepoHealth::Diverged);
    }
}
