use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::RepoInfo;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub scan_roots: Vec<String>,
}

pub fn data_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".gitatlas"))
}

fn ensure_dir() -> Option<PathBuf> {
    let dir = data_dir()?;
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

// Repo cache

/// Serialize the repo cache to JSON. Extracted so the round-trip is unit-testable
/// without touching the filesystem.
fn serialize_repos(repos: &[RepoInfo]) -> serde_json::Result<String> {
    serde_json::to_string(repos)
}

/// Deserialize the repo cache, falling back to an empty list on malformed data.
fn deserialize_repos(data: &str) -> Vec<RepoInfo> {
    serde_json::from_str(data).unwrap_or_default()
}

pub fn save(repos: &[RepoInfo]) {
    let Some(dir) = ensure_dir() else { return };
    let path = dir.join("cache.json");
    if let Ok(json) = serialize_repos(repos) {
        let _ = fs::write(&path, json);
    }
}

pub fn load() -> Vec<RepoInfo> {
    let Some(dir) = data_dir() else {
        return Vec::new();
    };
    let Ok(data) = fs::read_to_string(dir.join("cache.json")) else {
        return Vec::new();
    };
    deserialize_repos(&data)
}

// Config

/// Deserialize the config, falling back to defaults on malformed data.
fn deserialize_config(data: &str) -> Config {
    serde_json::from_str(data).unwrap_or_default()
}

pub fn load_config() -> Config {
    let Some(dir) = data_dir() else {
        return Config::default();
    };
    let Ok(data) = fs::read_to_string(dir.join("config.json")) else {
        return Config::default();
    };
    deserialize_config(&data)
}

pub fn save_config(config: &Config) {
    let Some(dir) = ensure_dir() else { return };
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = fs::write(dir.join("config.json"), json);
    }
}

/// Return effective scan roots: configured roots, or a default of ~/dev.
pub fn effective_scan_roots() -> Vec<PathBuf> {
    let cfg = load_config();
    if !cfg.scan_roots.is_empty() {
        return cfg.scan_roots.iter().map(PathBuf::from).collect();
    }
    if let Some(home) = dirs_next::home_dir() {
        let dev = home.join("dev");
        if dev.is_dir() {
            return vec![dev];
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RepoHealth;

    fn sample_repo(name: &str) -> RepoInfo {
        RepoInfo {
            path: format!("/tmp/{}", name),
            name: name.to_string(),
            branch: "main".to_string(),
            ahead: 1,
            behind: 2,
            dirty_files: 3,
            stash_count: 0,
            health: RepoHealth::Diverged,
            last_checked: "2026-05-20T00:00:00Z".to_string(),
            remote_url: Some("git@github.com:me/repo.git".to_string()),
        }
    }

    #[test]
    fn repo_cache_round_trips() {
        let repos = vec![sample_repo("alpha"), sample_repo("beta")];
        let json = serialize_repos(&repos).expect("serialize");
        let back = deserialize_repos(&json);

        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "alpha");
        assert_eq!(back[1].name, "beta");
        assert_eq!(back[0].ahead, 1);
        assert_eq!(back[0].behind, 2);
        assert_eq!(back[0].dirty_files, 3);
        assert_eq!(back[0].health, RepoHealth::Diverged);
        assert_eq!(back[0].remote_url.as_deref(), Some("git@github.com:me/repo.git"));
    }

    #[test]
    fn empty_cache_round_trips() {
        let json = serialize_repos(&[]).expect("serialize");
        assert_eq!(deserialize_repos(&json).len(), 0);
    }

    #[test]
    fn malformed_cache_yields_empty() {
        assert!(deserialize_repos("not json").is_empty());
        assert!(deserialize_repos("{\"unexpected\": true}").is_empty());
    }

    #[test]
    fn config_round_trips() {
        let cfg = Config {
            scan_roots: vec!["/a".to_string(), "/b".to_string()],
        };
        let json = serde_json::to_string_pretty(&cfg).expect("serialize");
        let back = deserialize_config(&json);
        assert_eq!(back.scan_roots, vec!["/a".to_string(), "/b".to_string()]);
    }

    #[test]
    fn malformed_config_yields_default() {
        assert!(deserialize_config("garbage").scan_roots.is_empty());
    }

    #[test]
    fn config_missing_scan_roots_defaults_empty() {
        // `scan_roots` uses #[serde(default)], so an empty object is valid.
        let back = deserialize_config("{}");
        assert!(back.scan_roots.is_empty());
    }
}
