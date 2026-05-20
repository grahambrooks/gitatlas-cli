use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoInfo {
    pub path: String,
    pub name: String,
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub dirty_files: u32,
    pub stash_count: u32,
    pub health: RepoHealth,
    pub last_checked: String,
    pub remote_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RepoHealth {
    Clean,
    Dirty,
    Diverged,
    Error,
}

impl RepoHealth {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "clean" => Some(RepoHealth::Clean),
            "dirty" | "changes" => Some(RepoHealth::Dirty),
            "diverged" => Some(RepoHealth::Diverged),
            "error" => Some(RepoHealth::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitInfo {
    pub oid: String,
    pub short_oid: String,
    pub message: String,
    pub author: String,
    pub author_email: String,
    pub date: String,
    pub parents: Vec<String>,
    pub refs: Vec<RefLabel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: FileStatus,
    pub staged: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl FileStatus {
    pub fn short(&self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "?",
            FileStatus::Conflicted => "U",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitFileChange {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteInfo {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitProfile {
    pub name: String,
    pub email: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_health_parse_known_values() {
        assert_eq!(RepoHealth::parse("clean"), Some(RepoHealth::Clean));
        assert_eq!(RepoHealth::parse("dirty"), Some(RepoHealth::Dirty));
        assert_eq!(RepoHealth::parse("changes"), Some(RepoHealth::Dirty));
        assert_eq!(RepoHealth::parse("diverged"), Some(RepoHealth::Diverged));
        assert_eq!(RepoHealth::parse("error"), Some(RepoHealth::Error));
    }

    #[test]
    fn repo_health_parse_is_case_insensitive() {
        assert_eq!(RepoHealth::parse("CLEAN"), Some(RepoHealth::Clean));
        assert_eq!(RepoHealth::parse("Dirty"), Some(RepoHealth::Dirty));
        assert_eq!(RepoHealth::parse("Diverged"), Some(RepoHealth::Diverged));
    }

    #[test]
    fn repo_health_parse_unknown_is_none() {
        assert_eq!(RepoHealth::parse(""), None);
        assert_eq!(RepoHealth::parse("healthy"), None);
        assert_eq!(RepoHealth::parse("conflict"), None);
    }

    #[test]
    fn repo_health_serde_uses_lowercase() {
        assert_eq!(
            serde_json::to_string(&RepoHealth::Diverged).unwrap(),
            "\"diverged\""
        );
        assert_eq!(
            serde_json::from_str::<RepoHealth>("\"clean\"").unwrap(),
            RepoHealth::Clean
        );
    }

    #[test]
    fn file_status_short_codes() {
        assert_eq!(FileStatus::Added.short(), "A");
        assert_eq!(FileStatus::Modified.short(), "M");
        assert_eq!(FileStatus::Deleted.short(), "D");
        assert_eq!(FileStatus::Renamed.short(), "R");
        assert_eq!(FileStatus::Untracked.short(), "?");
        assert_eq!(FileStatus::Conflicted.short(), "U");
    }

    #[test]
    fn file_status_serde_round_trip() {
        for status in [
            FileStatus::Added,
            FileStatus::Modified,
            FileStatus::Deleted,
            FileStatus::Renamed,
            FileStatus::Untracked,
            FileStatus::Conflicted,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: FileStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn ref_kind_serde_lowercase() {
        assert_eq!(serde_json::to_string(&RefKind::Head).unwrap(), "\"head\"");
        assert_eq!(serde_json::to_string(&RefKind::Remote).unwrap(), "\"remote\"");
    }
}
