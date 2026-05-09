use std::path::{Path, PathBuf};

use crate::cache;
use crate::error::{AppError, AppResult};
use crate::models::RepoInfo;

/// Resolve a user-provided identifier (a name from the cache, a path, or ".")
/// to a concrete repository path.
pub fn resolve(identifier: &str) -> AppResult<PathBuf> {
    // "." and paths: check directly
    if identifier == "." || identifier.starts_with('/') || identifier.starts_with("./")
        || identifier.starts_with("../") || identifier.starts_with('~')
    {
        return resolve_path(identifier);
    }

    // Try cache match by name
    let cached = cache::load();
    let matches: Vec<&RepoInfo> = cached
        .iter()
        .filter(|r| r.name.eq_ignore_ascii_case(identifier))
        .collect();

    match matches.len() {
        0 => {
            // Last resort: treat as a path
            let p = Path::new(identifier);
            if p.join(".git").exists() {
                return Ok(p.to_path_buf());
            }
            Err(AppError::msg(format!(
                "No repo found matching '{}'. Run `gitatlas scan` or pass a path.",
                identifier
            )))
        }
        1 => Ok(PathBuf::from(&matches[0].path)),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .map(|r| format!("  {} ({})", r.name, r.path))
                .collect();
            Err(AppError::msg(format!(
                "Multiple repos match '{}':\n{}\nSpecify the full path instead.",
                identifier,
                names.join("\n")
            )))
        }
    }
}

fn resolve_path(identifier: &str) -> AppResult<PathBuf> {
    let expanded = if let Some(stripped) = identifier.strip_prefix("~/") {
        match dirs_next::home_dir() {
            Some(home) => home.join(stripped),
            None => PathBuf::from(identifier),
        }
    } else if identifier == "~" {
        dirs_next::home_dir().ok_or_else(|| AppError::msg("Cannot resolve home directory"))?
    } else {
        PathBuf::from(identifier)
    };

    let canonical = expanded
        .canonicalize()
        .map_err(|e| AppError::msg(format!("Cannot resolve '{}': {}", identifier, e)))?;

    if !canonical.join(".git").exists() {
        return Err(AppError::msg(format!(
            "Path '{}' is not a git repository",
            canonical.display()
        )));
    }

    Ok(canonical)
}

/// Resolve a list of identifiers (for bulk operations). If empty and `all` is
/// true, returns every cached repo.
pub fn resolve_many(identifiers: &[String], all: bool) -> AppResult<Vec<(String, PathBuf)>> {
    if all {
        let cached = cache::load();
        if cached.is_empty() {
            return Err(AppError::msg(
                "Cache is empty. Run `gitatlas scan` first.",
            ));
        }
        return Ok(cached
            .into_iter()
            .map(|r| (r.name, PathBuf::from(r.path)))
            .collect());
    }

    if identifiers.is_empty() {
        return Err(AppError::msg(
            "Specify one or more repos, or pass --all.",
        ));
    }

    let mut out = Vec::new();
    for id in identifiers {
        let path = resolve(id)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| id.clone());
        out.push((name, path));
    }
    Ok(out)
}
