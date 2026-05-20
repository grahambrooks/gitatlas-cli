use std::path::{Path, PathBuf};

use crate::cache;
use crate::error::{AppError, AppResult};
use crate::models::RepoInfo;

/// Resolve a user-provided identifier (a name from the cache, a path, or ".")
/// to a concrete repository path.
pub fn resolve(identifier: &str) -> AppResult<PathBuf> {
    // "." and paths: check directly
    if looks_like_path(identifier) {
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
            return Err(AppError::msg("Cache is empty. Run `gitatlas scan` first."));
        }
        return Ok(cached
            .into_iter()
            .map(|r| (r.name, PathBuf::from(r.path)))
            .collect());
    }

    if identifiers.is_empty() {
        return Err(AppError::msg("Specify one or more repos, or pass --all."));
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

/// True if an identifier should be treated as a filesystem path rather than a
/// cache lookup by name.
fn looks_like_path(identifier: &str) -> bool {
    identifier == "."
        || identifier.starts_with('/')
        || identifier.starts_with("./")
        || identifier.starts_with("../")
        || identifier.starts_with('~')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "gitatlas-test-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn path_detection() {
        assert!(looks_like_path("."));
        assert!(looks_like_path("/abs/path"));
        assert!(looks_like_path("./rel"));
        assert!(looks_like_path("../up"));
        assert!(looks_like_path("~/home/repo"));
        assert!(looks_like_path("~"));

        assert!(!looks_like_path("myrepo"));
        assert!(!looks_like_path("some-name"));
    }

    #[test]
    fn resolve_path_accepts_git_dir() {
        let dir = unique_temp_dir("resolve-ok");
        fs::create_dir_all(dir.join(".git")).expect("create .git");

        let resolved = resolve_path(dir.to_str().unwrap()).expect("should resolve");
        assert!(resolved.join(".git").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_path_rejects_non_git_dir() {
        let dir = unique_temp_dir("resolve-nongit");

        let err = resolve_path(dir.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_path_missing_path_errors() {
        let missing = std::env::temp_dir().join("gitatlas-definitely-missing-xyz");
        let err = resolve_path(missing.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("Cannot resolve"));
    }

    #[test]
    fn resolve_many_empty_without_all_errors() {
        let err = resolve_many(&[], false).unwrap_err();
        assert!(err.to_string().contains("Specify one or more repos"));
    }
}
