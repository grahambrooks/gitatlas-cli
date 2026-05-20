use git2::{DiffOptions, Repository, Sort, StatusOptions, StatusShow};
use std::collections::HashMap;
use std::path::Path;

use crate::error::AppError;
use crate::models::{
    BranchInfo, CommitFileChange, CommitInfo, FileChange, FileStatus, GitProfile, RefKind,
    RefLabel, RemoteInfo, StashEntry,
};

// Commit log

pub fn get_commit_log(path: &Path, count: usize) -> Result<Vec<CommitInfo>, AppError> {
    let repo = Repository::open(path)?;

    let ref_map = build_ref_map(&repo);

    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;

    if let Ok(head) = repo.head() {
        if let Some(oid) = head.target() {
            let _ = revwalk.push(oid);
        }
    }
    for (branch, _) in repo.branches(None).into_iter().flatten().flatten() {
        if let Ok(reference) = branch.into_reference().resolve() {
            if let Some(oid) = reference.target() {
                let _ = revwalk.push(oid);
            }
        }
    }

    let mut commits = Vec::new();
    for oid_result in revwalk.take(count) {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;
        let oid_str = oid.to_string();
        let short = oid_str[..7].to_string();

        let refs = ref_map.get(&oid_str).cloned().unwrap_or_default();

        commits.push(CommitInfo {
            oid: oid_str,
            short_oid: short,
            message: commit.message().unwrap_or("").trim().to_string(),
            author: commit.author().name().unwrap_or("Unknown").to_string(),
            author_email: commit.author().email().unwrap_or("").to_string(),
            date: chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            parents: commit
                .parent_ids()
                .map(|id| id.to_string()[..7].to_string())
                .collect(),
            refs,
        });
    }

    Ok(commits)
}

fn build_ref_map(repo: &Repository) -> HashMap<String, Vec<RefLabel>> {
    let mut map: HashMap<String, Vec<RefLabel>> = HashMap::new();

    let head_oid = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| o.to_string());
    let head_name = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(String::from));

    if let Ok(branches) = repo.branches(None) {
        for (branch, branch_type) in branches.flatten() {
            let name = match branch.name() {
                Ok(Some(n)) => n.to_string(),
                _ => continue,
            };
            let reference = match branch.into_reference().resolve() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let oid = match reference.target() {
                Some(o) => o.to_string(),
                None => continue,
            };

            let is_head =
                head_oid.as_deref() == Some(&oid) && head_name.as_deref() == Some(&name);

            let kind = if is_head {
                RefKind::Head
            } else if branch_type == git2::BranchType::Remote {
                RefKind::Remote
            } else {
                RefKind::Local
            };

            map.entry(oid).or_default().push(RefLabel { name, kind });
        }
    }

    if let Ok(tag_names) = repo.tag_names(None) {
        for tag_name in tag_names.iter().flatten() {
            if let Ok(reference) = repo.find_reference(&format!("refs/tags/{}", tag_name)) {
                let oid = reference
                    .peel(git2::ObjectType::Commit)
                    .ok()
                    .map(|obj| obj.id().to_string());
                if let Some(oid) = oid {
                    map.entry(oid).or_default().push(RefLabel {
                        name: tag_name.to_string(),
                        kind: RefKind::Tag,
                    });
                }
            }
        }
    }

    map
}

// File changes

pub fn get_file_changes(path: &Path) -> Result<Vec<FileChange>, AppError> {
    let repo = Repository::open(path)?;
    let mut changes = Vec::new();

    let mut staged_opts = StatusOptions::new();
    staged_opts.show(StatusShow::Index);
    staged_opts.include_untracked(false);
    let staged_statuses = repo.statuses(Some(&mut staged_opts))?;

    for entry in staged_statuses.iter() {
        let status = entry.status();
        let file_path = entry.path().unwrap_or("").to_string();
        if let Some(fs) = index_status_to_file_status(status) {
            changes.push(FileChange {
                path: file_path,
                status: fs,
                staged: true,
            });
        }
    }

    let mut unstaged_opts = StatusOptions::new();
    unstaged_opts.show(StatusShow::Workdir);
    unstaged_opts.include_untracked(true);
    unstaged_opts.recurse_untracked_dirs(false);
    let unstaged_statuses = repo.statuses(Some(&mut unstaged_opts))?;

    for entry in unstaged_statuses.iter() {
        let status = entry.status();
        let file_path = entry.path().unwrap_or("").to_string();
        if let Some(fs) = workdir_status_to_file_status(status) {
            changes.push(FileChange {
                path: file_path,
                status: fs,
                staged: false,
            });
        }
    }

    Ok(changes)
}

fn index_status_to_file_status(s: git2::Status) -> Option<FileStatus> {
    if s.contains(git2::Status::INDEX_NEW) {
        Some(FileStatus::Added)
    } else if s.contains(git2::Status::INDEX_MODIFIED) {
        Some(FileStatus::Modified)
    } else if s.contains(git2::Status::INDEX_DELETED) {
        Some(FileStatus::Deleted)
    } else if s.contains(git2::Status::INDEX_RENAMED) {
        Some(FileStatus::Renamed)
    } else if s.contains(git2::Status::CONFLICTED) {
        Some(FileStatus::Conflicted)
    } else {
        None
    }
}

fn workdir_status_to_file_status(s: git2::Status) -> Option<FileStatus> {
    if s.contains(git2::Status::WT_NEW) {
        Some(FileStatus::Untracked)
    } else if s.contains(git2::Status::WT_MODIFIED) {
        Some(FileStatus::Modified)
    } else if s.contains(git2::Status::WT_DELETED) {
        Some(FileStatus::Deleted)
    } else if s.contains(git2::Status::WT_RENAMED) {
        Some(FileStatus::Renamed)
    } else if s.contains(git2::Status::CONFLICTED) {
        Some(FileStatus::Conflicted)
    } else {
        None
    }
}

// Diff

pub fn get_file_diff(path: &Path, file_path: &str, staged: bool) -> Result<String, AppError> {
    let repo = Repository::open(path)?;

    let mut diff_opts = DiffOptions::new();
    diff_opts.pathspec(file_path);
    diff_opts.context_lines(3);

    let diff = if staged {
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut diff_opts))?
    };

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            _ => "",
        };
        output.push_str(prefix);
        if let Ok(content) = std::str::from_utf8(line.content()) {
            output.push_str(content);
        }
        true
    })?;

    if output.is_empty() {
        output = "(no diff available)".to_string();
    }

    Ok(output)
}

/// Full working-tree diff (all paths).
pub fn get_workdir_diff(path: &Path, staged: bool) -> Result<String, AppError> {
    let repo = Repository::open(path)?;
    let mut diff_opts = DiffOptions::new();
    diff_opts.context_lines(3);

    let diff = if staged {
        let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut diff_opts))?
    };

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            _ => "",
        };
        output.push_str(prefix);
        if let Ok(content) = std::str::from_utf8(line.content()) {
            output.push_str(content);
        }
        true
    })?;

    Ok(output)
}

pub fn get_commit_diff(path: &Path, oid_str: &str) -> Result<String, AppError> {
    let repo = Repository::open(path)?;
    let oid = git2::Oid::from_str(oid_str)?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;

    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            'F' => "",
            'H' => "",
            _ => "",
        };
        output.push_str(prefix);
        if let Ok(content) = std::str::from_utf8(line.content()) {
            output.push_str(content);
        }
        true
    })?;

    if output.is_empty() {
        output = "(empty commit)".to_string();
    }

    Ok(output)
}

// Staging

pub fn stage_files(path: &Path, files: &[String]) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let mut index = repo.index()?;

    for file in files {
        let file_path = Path::new(file);
        if path.join(file_path).exists() {
            index.add_path(file_path)?;
        } else {
            index.remove_path(file_path)?;
        }
    }

    index.write()?;
    Ok(())
}

pub fn unstage_files(path: &Path, files: &[String]) -> Result<(), AppError> {
    let repo = Repository::open(path)?;

    let head = repo.head();
    match head {
        Ok(head_ref) => {
            let head_commit = head_ref.peel_to_commit()?;
            let head_tree = head_commit.tree()?;
            repo.reset_default(Some(head_tree.as_object()), files.iter().map(Path::new))?;
        }
        Err(_) => {
            let mut index = repo.index()?;
            for file in files {
                index.remove_path(Path::new(file))?;
            }
            index.write()?;
        }
    }

    Ok(())
}

pub fn stage_all(path: &Path) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let mut index = repo.index()?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    Ok(())
}

pub fn unstage_all(path: &Path) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let head = repo.head();

    match head {
        Ok(head_ref) => {
            let obj = head_ref.peel(git2::ObjectType::Commit)?;
            repo.reset(&obj, git2::ResetType::Mixed, None)?;
        }
        Err(_) => {
            let mut index = repo.index()?;
            index.clear()?;
            index.write()?;
        }
    }

    Ok(())
}

// Commit

pub fn create_commit(path: &Path, message: &str) -> Result<String, AppError> {
    let repo = Repository::open(path)?;
    let sig = repo.signature()?;
    let mut index = repo.index()?;
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;

    let parents = match repo.head() {
        Ok(head) => vec![head.peel_to_commit()?],
        Err(_) => vec![],
    };
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    let oid = repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parent_refs)?;

    Ok(oid.to_string())
}

// Branches

pub fn get_branches(path: &Path) -> Result<Vec<BranchInfo>, AppError> {
    let repo = Repository::open(path)?;
    let mut branches = Vec::new();

    let head_name = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(String::from));

    for branch_result in repo.branches(None)? {
        let (branch, branch_type) = branch_result?;
        let name = match branch.name()? {
            Some(n) => n.to_string(),
            None => continue,
        };
        let is_remote = branch_type == git2::BranchType::Remote;
        let is_head = head_name.as_deref() == Some(&name);

        let upstream = branch
            .upstream()
            .ok()
            .and_then(|u| u.name().ok().flatten().map(String::from));

        branches.push(BranchInfo {
            name,
            is_head,
            is_remote,
            upstream,
        });
    }

    branches.sort_by(|a, b| {
        b.is_head
            .cmp(&a.is_head)
            .then(a.is_remote.cmp(&b.is_remote))
            .then(a.name.cmp(&b.name))
    });

    Ok(branches)
}

pub fn checkout_branch(path: &Path, branch_name: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let (object, reference) = repo.revparse_ext(branch_name)?;
    repo.checkout_tree(&object, None)?;

    if let Some(reference) = reference {
        let ref_name = reference
            .name()
            .ok_or_else(|| AppError::msg("Invalid reference name"))?;
        repo.set_head(ref_name)?;
    } else {
        repo.set_head_detached(object.id())?;
    }

    Ok(())
}

pub fn create_branch(path: &Path, branch_name: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let head = repo.head()?;
    let commit = head.peel_to_commit()?;
    repo.branch(branch_name, &commit, false)?;
    Ok(())
}

pub fn delete_branch(path: &Path, branch_name: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let mut branch = repo.find_branch(branch_name, git2::BranchType::Local)?;
    branch.delete()?;
    Ok(())
}

// Stashes

pub fn get_stashes(path: &Path) -> Result<Vec<StashEntry>, AppError> {
    let mut repo = Repository::open(path)?;
    let mut stashes = Vec::new();

    repo.stash_foreach(|index, message, _oid| {
        stashes.push(StashEntry {
            index,
            message: message.to_string(),
        });
        true
    })?;

    Ok(stashes)
}

pub fn stash_save(path: &Path, message: &str) -> Result<(), AppError> {
    let mut repo = Repository::open(path)?;
    let sig = repo.signature()?;
    let msg = if message.is_empty() { "WIP" } else { message };
    repo.stash_save(&sig, msg, None)?;
    Ok(())
}

pub fn stash_pop(path: &Path, index: usize) -> Result<(), AppError> {
    let mut repo = Repository::open(path)?;
    repo.stash_pop(index, None)?;
    Ok(())
}

pub fn stash_drop(path: &Path, index: usize) -> Result<(), AppError> {
    let mut repo = Repository::open(path)?;
    repo.stash_drop(index)?;
    Ok(())
}

// Commit files

pub fn get_commit_files(path: &Path, oid_str: &str) -> Result<Vec<CommitFileChange>, AppError> {
    let repo = Repository::open(path)?;
    let oid = git2::Oid::from_str(oid_str)?;
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;

    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;

    let mut files = Vec::new();
    for delta in diff.deltas() {
        let status = match delta.status() {
            git2::Delta::Added => "added",
            git2::Delta::Deleted => "deleted",
            git2::Delta::Modified => "modified",
            git2::Delta::Renamed => "renamed",
            git2::Delta::Copied => "copied",
            _ => "modified",
        };
        let file_path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        files.push(CommitFileChange {
            path: file_path,
            status: status.to_string(),
        });
    }

    Ok(files)
}

// Merge

pub fn merge_branch(path: &Path, branch_name: &str) -> Result<String, AppError> {
    let repo = Repository::open(path)?;
    let branch_ref = repo
        .find_branch(branch_name, git2::BranchType::Local)
        .or_else(|_| repo.find_branch(branch_name, git2::BranchType::Remote))?;
    let annotated = repo.reference_to_annotated_commit(branch_ref.get())?;

    let (analysis, _) = repo.merge_analysis(&[&annotated])?;

    if analysis.is_up_to_date() {
        return Ok("Already up to date".to_string());
    }

    if analysis.is_fast_forward() {
        let target_oid = annotated.id();
        let target_obj = repo.find_object(target_oid, None)?;
        repo.checkout_tree(&target_obj, None)?;

        let mut head_ref = repo.head()?;
        head_ref.set_target(target_oid, &format!("Fast-forward merge {}", branch_name))?;

        return Ok("Fast-forward merge".to_string());
    }

    repo.merge(&[&annotated], None, None)?;

    let sig = repo.signature()?;
    let mut index = repo.index()?;
    if index.has_conflicts() {
        repo.cleanup_state()?;
        return Err(AppError::msg("Merge has conflicts — resolve them manually"));
    }
    let tree_oid = index.write_tree()?;
    let tree = repo.find_tree(tree_oid)?;
    let head_commit = repo.head()?.peel_to_commit()?;
    let merge_commit = repo.find_commit(annotated.id())?;
    let msg = format!("Merge branch '{}'", branch_name);
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        &msg,
        &tree,
        &[&head_commit, &merge_commit],
    )?;
    repo.cleanup_state()?;

    Ok("Merge commit created".to_string())
}

// File history

pub fn get_file_history(
    path: &Path,
    file_path: &str,
    count: usize,
) -> Result<Vec<CommitInfo>, AppError> {
    let repo = Repository::open(path)?;
    let ref_map = build_ref_map(&repo);

    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;
    if let Ok(head) = repo.head() {
        if let Some(oid) = head.target() {
            let _ = revwalk.push(oid);
        }
    }

    let mut commits = Vec::new();
    let mut prev_blob_id: Option<git2::Oid> = None;

    for oid_result in revwalk {
        let oid = oid_result?;
        let commit = repo.find_commit(oid)?;
        let tree = commit.tree()?;

        let blob_id = tree
            .get_path(Path::new(file_path))
            .ok()
            .map(|entry| entry.id());

        let include = if prev_blob_id.is_none() {
            blob_id.is_some()
        } else {
            blob_id != prev_blob_id
        };

        if include {
            let oid_str = oid.to_string();
            let short = oid_str[..7].to_string();
            let refs = ref_map.get(&oid_str).cloned().unwrap_or_default();

            commits.push(CommitInfo {
                oid: oid_str,
                short_oid: short,
                message: commit.message().unwrap_or("").trim().to_string(),
                author: commit.author().name().unwrap_or("Unknown").to_string(),
                author_email: commit.author().email().unwrap_or("").to_string(),
                date: chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                parents: commit
                    .parent_ids()
                    .map(|id| id.to_string()[..7].to_string())
                    .collect(),
                refs,
            });

            if commits.len() >= count {
                break;
            }
        }

        prev_blob_id = blob_id;
    }

    Ok(commits)
}

// Remotes

pub fn get_remotes(path: &Path) -> Result<Vec<RemoteInfo>, AppError> {
    let repo = Repository::open(path)?;
    let remote_names = repo.remotes()?;
    let mut remotes = Vec::new();

    for name in remote_names.iter().flatten() {
        let remote = repo.find_remote(name)?;
        let url = remote.url().unwrap_or("").to_string();
        remotes.push(RemoteInfo {
            name: name.to_string(),
            url,
        });
    }

    Ok(remotes)
}

pub fn add_remote(path: &Path, name: &str, url: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    repo.remote(name, url)?;
    Ok(())
}

pub fn remove_remote(path: &Path, name: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    repo.remote_delete(name)?;
    Ok(())
}

pub fn rename_remote(path: &Path, old_name: &str, new_name: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    repo.remote_rename(old_name, new_name)?;
    Ok(())
}

// Git profile

pub fn get_git_profile(path: &Path) -> Result<GitProfile, AppError> {
    let repo = Repository::open(path)?;
    let config = repo.config()?;

    let name = config.get_string("user.name").unwrap_or_default();
    let email = config.get_string("user.email").unwrap_or_default();

    Ok(GitProfile { name, email })
}

pub fn set_git_profile(path: &Path, name: &str, email: &str) -> Result<(), AppError> {
    let repo = Repository::open(path)?;
    let mut config = repo.config()?;

    config.set_str("user.name", name)?;
    config.set_str("user.email", email)?;

    Ok(())
}

// Squash

pub fn squash_commits(path: &Path, count: usize, message: &str) -> Result<String, AppError> {
    if count < 2 {
        return Err(AppError::msg("Need at least 2 commits to squash"));
    }

    let repo = Repository::open(path)?;

    let head_commit = repo.head()?.peel_to_commit()?;
    let head_tree = head_commit.tree()?;

    let mut current = head_commit.clone();
    for _ in 1..count {
        if current.parent_count() == 0 {
            return Err(AppError::msg("Not enough commits to squash"));
        }
        if current.parent_count() > 1 {
            return Err(AppError::msg("Cannot squash across merge commits"));
        }
        current = current.parent(0)?;
    }
    let base_parents: Vec<git2::Commit> = (0..current.parent_count())
        .filter_map(|i| current.parent(i).ok())
        .collect();
    let parent_refs: Vec<&git2::Commit> = base_parents.iter().collect();

    let sig = repo.signature()?;
    let new_oid = repo.commit(None, &sig, &sig, message, &head_tree, &parent_refs)?;

    let new_commit = repo.find_object(new_oid, None)?;
    repo.reset(&new_commit, git2::ResetType::Hard, None)?;

    Ok(new_oid.to_string())
}

// PR URL

pub fn get_pr_url(path: &Path) -> Result<String, AppError> {
    let repo = Repository::open(path)?;
    let remote = repo
        .find_remote("origin")
        .map_err(|_| AppError::msg("No 'origin' remote found"))?;
    let url = remote
        .url()
        .ok_or_else(|| AppError::msg("Origin remote has no URL"))?;

    let base = normalize_remote_url(url)?;

    let head = repo.head()?;
    let branch = head.shorthand().unwrap_or("main");

    let pr_url = if base.contains("github.com") {
        format!("{}/compare/{}?expand=1", base, branch)
    } else if base.contains("gitlab.com") || base.contains("gitlab") {
        format!(
            "{}/-/merge_requests/new?merge_request%5Bsource_branch%5D={}",
            base, branch
        )
    } else if base.contains("bitbucket.org") {
        format!("{}/pull-requests/new?source={}", base, branch)
    } else {
        return Err(AppError::msg(format!(
            "Unsupported git host in URL: {}",
            url
        )));
    };

    Ok(pr_url)
}

fn normalize_remote_url(url: &str) -> Result<String, AppError> {
    if let Some(rest) = url.strip_prefix("git@") {
        let normalized = rest.replace(':', "/");
        let trimmed = normalized.strip_suffix(".git").unwrap_or(&normalized);
        return Ok(format!("https://{}", trimmed));
    }

    let trimmed = url.strip_suffix(".git").unwrap_or(url);
    Ok(trimmed.to_string())
}

// README

pub fn get_readme(path: &Path) -> Result<Option<String>, AppError> {
    let candidates = [
        "README.md",
        "readme.md",
        "Readme.md",
        "README.MD",
        "README",
        "README.txt",
        "README.rst",
    ];

    for name in &candidates {
        let file_path = path.join(name);
        if file_path.is_file() {
            let content = std::fs::read_to_string(&file_path)
                .map_err(|e| AppError::msg(format!("Failed to read {}: {}", name, e)))?;
            return Ok(Some(content));
        }
    }

    Ok(None)
}
