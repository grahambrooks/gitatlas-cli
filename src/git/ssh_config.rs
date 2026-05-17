//! Minimal `~/.ssh/config` integration so credential resolution honors
//! `Host` aliases, `HostName`, `User`, `Port`, `IdentityFile`, and
//! `IdentitiesOnly` directives. libgit2/libssh2 do not read ssh_config on
//! their own.

use std::io::BufReader;
use std::path::PathBuf;
use std::sync::OnceLock;

use ssh2_config::{ParseRule, SshConfig};

#[derive(Debug, Clone, Default)]
pub struct ResolvedHost {
    /// Effective hostname after `HostName` rewriting. Falls back to the input.
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// `IdentityFile` entries in declaration order, with `~` expanded.
    pub identity_files: Vec<PathBuf>,
    /// `IdentitiesOnly yes` — only configured keys should be tried.
    pub identities_only: bool,
}

fn config() -> Option<&'static SshConfig> {
    static CONFIG: OnceLock<Option<SshConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let home = std::env::var("HOME").ok()?;
            let path = PathBuf::from(home).join(".ssh").join("config");
            if !path.exists() {
                return None;
            }
            let file = std::fs::File::open(&path).ok()?;
            let mut reader = BufReader::new(file);
            SshConfig::default()
                .parse(&mut reader, ParseRule::ALLOW_UNKNOWN_FIELDS)
                .ok()
        })
        .as_ref()
}

pub fn resolve(host: &str) -> ResolvedHost {
    let mut out = ResolvedHost {
        hostname: host.to_string(),
        ..Default::default()
    };

    let Some(cfg) = config() else {
        return out;
    };

    let params = cfg.query(host);

    if let Some(h) = params.host_name {
        out.hostname = h;
    }
    out.user = params.user;
    out.port = params.port;
    if let Some(files) = params.identity_file {
        out.identity_files = files.into_iter().map(expand_tilde).collect();
    }

    // ssh2-config 0.7 doesn't surface `IdentitiesOnly` as a typed field; it
    // lands in `unsupported_fields`. Read it from there.
    if let Some(values) = params.unsupported_fields.get("identitiesonly") {
        if let Some(v) = values.first() {
            out.identities_only = matches!(
                v.to_ascii_lowercase().as_str(),
                "yes" | "true" | "on" | "1"
            );
        }
    }

    out
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path
}
