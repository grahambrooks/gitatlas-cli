use git2::{
    AnnotatedCommit, AutotagOption, Cred, CredentialType, FetchOptions, PushOptions,
    RemoteCallbacks, Repository,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Once;

use crate::error::AppError;
use crate::git::ssh_config::{self, ResolvedHost};

#[derive(Debug, Clone)]
pub enum ProgressEvent {
    /// High-level phase, e.g. "fetching origin", "rebasing onto origin/main".
    Stage(String),
    /// Object transfer stats from the network side.
    Transfer {
        received_bytes: usize,
        indexed_objects: usize,
        received_objects: usize,
        total_objects: usize,
    },
    /// Push-side byte counter.
    PushTransfer {
        current: usize,
        total: usize,
        bytes: usize,
    },
    /// Free-form line from the remote (server `remote:` messages).
    Sideband(String),
    /// A ref was updated locally (refspec-style name, old→new short oids).
    Tip(String),
    /// One step of a rebase finished.
    RebaseStep { current: usize, total: usize },
}

pub type ProgressTx = Sender<ProgressEvent>;

fn emit(tx: Option<&ProgressTx>, event: ProgressEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event);
    }
}

/// Ensure SSH_AUTH_SOCK is set (for macOS GUI-launched processes). Harmless in
/// CLI context but preserved for consistency with the gitatlas desktop app.
fn ensure_ssh_auth_sock() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if std::env::var("SSH_AUTH_SOCK").is_ok() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir("/private/tmp") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("com.apple.launchd.") {
                    let sock = entry.path().join("Listeners");
                    if sock.exists() {
                        std::env::set_var("SSH_AUTH_SOCK", &sock);
                        return;
                    }
                }
            }
        }
    });
}

fn make_callbacks(tx: Option<ProgressTx>, host: Option<ResolvedHost>) -> RemoteCallbacks<'static> {
    ensure_ssh_auth_sock();

    let mut callbacks = RemoteCallbacks::new();

    // Credentials are always handled by a stateful, attempt-limited resolver,
    // regardless of whether progress reporting is requested. Without state,
    // libgit2 loops forever when the server rejects a key: it re-invokes the
    // callback, we return the same credential, and the cycle repeats.
    let cred_tx = tx.clone();
    let mut state = CredState::new(host);
    callbacks.credentials(move |url, username_from_url, allowed_types| {
        try_next_credential(
            &mut state,
            url,
            username_from_url,
            allowed_types,
            cred_tx.as_ref(),
        )
    });

    if let Some(tx) = tx.clone() {
        let xfer_tx = tx.clone();
        callbacks.transfer_progress(move |stats| {
            let _ = xfer_tx.send(ProgressEvent::Transfer {
                received_bytes: stats.received_bytes(),
                indexed_objects: stats.indexed_objects(),
                received_objects: stats.received_objects(),
                total_objects: stats.total_objects(),
            });
            true
        });

        let sb_tx = tx.clone();
        callbacks.sideband_progress(move |data| {
            if let Ok(s) = std::str::from_utf8(data) {
                for line in s.split(['\n', '\r']) {
                    let line = line.trim();
                    if !line.is_empty() {
                        let _ = sb_tx.send(ProgressEvent::Sideband(line.to_string()));
                    }
                }
            }
            true
        });

        let tip_tx = tx.clone();
        callbacks.update_tips(move |refname, old, new| {
            let label = if old.is_zero() {
                format!("{} (new)", refname)
            } else if new.is_zero() {
                format!("{} (deleted)", refname)
            } else {
                format!("{} {:.7}..{:.7}", refname, old, new)
            };
            let _ = tip_tx.send(ProgressEvent::Tip(label));
            true
        });

        let push_tx = tx;
        callbacks.push_transfer_progress(move |current, total, bytes| {
            let _ = push_tx.send(ProgressEvent::PushTransfer {
                current,
                total,
                bytes,
            });
        });
    }

    callbacks
}

/// Per-connection credential state. libgit2 calls the credentials callback
/// repeatedly when an offered credential is rejected; we walk through the
/// available methods once each and then return `Err` so libgit2 gives up
/// instead of looping forever.
struct CredState {
    attempts: usize,
    /// SSH private-key paths to try, in order. When the resolved host has
    /// `IdentitiesOnly yes`, this is exactly the configured `IdentityFile`s;
    /// otherwise it's the configured files followed by discovered defaults.
    ssh_keys: Vec<PathBuf>,
    ssh_key_idx: usize,
    /// When true, skip the agent entirely — only the `ssh_keys` list is used.
    /// Mirrors `IdentitiesOnly yes` from ssh_config.
    identities_only: bool,
    /// Preferred username from ssh_config (`User` directive), used when the
    /// URL doesn't carry one.
    configured_user: Option<String>,
    tried_agent: bool,
    tried_helper: bool,
    tried_username_only: bool,
    tried_default: bool,
}

impl CredState {
    fn new(host: Option<ResolvedHost>) -> Self {
        let (ssh_keys, identities_only, configured_user) = match host {
            Some(h) => {
                let mut keys: Vec<PathBuf> = h.identity_files.clone();
                if !h.identities_only {
                    let mut seen: std::collections::HashSet<PathBuf> =
                        keys.iter().cloned().collect();
                    for k in discover_ssh_keys() {
                        if seen.insert(k.clone()) {
                            keys.push(k);
                        }
                    }
                }
                (keys, h.identities_only, h.user)
            }
            None => (discover_ssh_keys(), false, None),
        };
        Self {
            attempts: 0,
            ssh_keys,
            ssh_key_idx: 0,
            identities_only,
            configured_user,
            tried_agent: false,
            tried_helper: false,
            tried_username_only: false,
            tried_default: false,
        }
    }
}

/// Discover candidate SSH private keys in `~/.ssh/`. Includes the standard
/// `id_*` names plus any other file that has a matching `.pub` sibling.
/// Note: this does **not** parse `~/.ssh/config` (libssh2 doesn't either) —
/// `Host`/`IdentityFile`/`IdentitiesOnly` directives are ignored.
fn discover_ssh_keys() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
    let ssh_dir = PathBuf::from(&home).join(".ssh");
    let mut keys: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Standard names first, in preference order.
    for name in &[
        "id_ed25519",
        "id_ed25519_sk",
        "id_ecdsa",
        "id_ecdsa_sk",
        "id_rsa",
        "id_dsa",
    ] {
        let p = ssh_dir.join(name);
        if p.exists() && seen.insert(p.clone()) {
            keys.push(p);
        }
    }

    // Then any other file that has a `.pub` sibling — catches user-named keys
    // like `id_ed25519_personal`, `github_work`, etc.
    if let Ok(entries) = std::fs::read_dir(&ssh_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("pub") {
                let priv_path = path.with_extension("");
                if priv_path.exists() && seen.insert(priv_path.clone()) {
                    keys.push(priv_path);
                }
            }
        }
    }

    keys
}

fn try_next_credential(
    state: &mut CredState,
    url: &str,
    username_from_url: Option<&str>,
    allowed_types: CredentialType,
    tx: Option<&ProgressTx>,
) -> Result<Cred, git2::Error> {
    state.attempts += 1;
    // Hard cap as a final safety net.
    if state.attempts > 16 {
        return Err(git2::Error::from_str(
            "auth: too many credential attempts — giving up",
        ));
    }

    if allowed_types.contains(CredentialType::SSH_KEY) {
        let user = username_from_url
            .map(|u| u.to_string())
            .or_else(|| state.configured_user.clone())
            .unwrap_or_else(|| "git".to_string());

        if !state.tried_agent && !state.identities_only {
            state.tried_agent = true;
            emit(
                tx,
                ProgressEvent::Stage(format!("auth: ssh-agent ({})", user)),
            );
            if let Ok(cred) = Cred::ssh_key_from_agent(&user) {
                return Ok(cred);
            }
        }

        while state.ssh_key_idx < state.ssh_keys.len() {
            let key = state.ssh_keys[state.ssh_key_idx].clone();
            state.ssh_key_idx += 1;
            let pub_path = {
                let mut p = key.clone();
                p.set_extension("pub");
                p
            };
            emit(tx, ProgressEvent::Stage(format!("auth: {}", key.display())));
            let pub_arg = if pub_path.exists() {
                Some(pub_path)
            } else {
                None
            };
            // `None` passphrase means encrypted keys will fail here; we'll
            // just advance to the next key.
            if let Ok(cred) = Cred::ssh_key(&user, pub_arg.as_deref(), &key, None) {
                return Ok(cred);
            }
        }
    }

    if allowed_types.contains(CredentialType::SSH_MEMORY)
        && !state.tried_agent
        && !state.identities_only
    {
        // Some servers advertise SSH_MEMORY without SSH_KEY; still try the agent.
        state.tried_agent = true;
        let user = username_from_url
            .map(|u| u.to_string())
            .or_else(|| state.configured_user.clone())
            .unwrap_or_else(|| "git".to_string());
        emit(
            tx,
            ProgressEvent::Stage(format!("auth: ssh-agent ({})", user)),
        );
        if let Ok(cred) = Cred::ssh_key_from_agent(&user) {
            return Ok(cred);
        }
    }

    if allowed_types.contains(CredentialType::USER_PASS_PLAINTEXT) && !state.tried_helper {
        state.tried_helper = true;
        emit(tx, ProgressEvent::Stage("auth: credential helper".into()));
        if let Ok(cred) = Cred::credential_helper(
            &git2::Config::open_default()
                .unwrap_or_else(|_| git2::Config::new().expect("failed to create git config")),
            url,
            username_from_url,
        ) {
            return Ok(cred);
        }
    }

    if allowed_types.contains(CredentialType::USERNAME) && !state.tried_username_only {
        state.tried_username_only = true;
        if let Some(user) = username_from_url {
            emit(
                tx,
                ProgressEvent::Stage(format!("auth: username ({})", user)),
            );
            if let Ok(cred) = Cred::username(user) {
                return Ok(cred);
            }
        }
    }

    if allowed_types.contains(CredentialType::DEFAULT) && !state.tried_default {
        state.tried_default = true;
        emit(tx, ProgressEvent::Stage("auth: default".into()));
        return Cred::default();
    }

    // Build a useful error message describing what we tried.
    let mut tried: Vec<&str> = Vec::new();
    if state.tried_agent {
        tried.push("ssh-agent");
    }
    if state.ssh_key_idx > 0 {
        tried.push("~/.ssh keys");
    }
    if state.tried_helper {
        tried.push("credential helper");
    }
    if state.tried_default {
        tried.push("default");
    }
    let summary = if tried.is_empty() {
        "no usable authentication method advertised by server".to_string()
    } else {
        format!("all methods rejected ({})", tried.join(", "))
    };
    Err(git2::Error::from_str(&format!("auth: {}", summary)))
}

fn make_fetch_options(tx: Option<ProgressTx>, host: Option<ResolvedHost>) -> FetchOptions<'static> {
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(make_callbacks(tx, host));
    fetch_opts.download_tags(AutotagOption::All);
    fetch_opts
}

fn make_push_options(tx: Option<ProgressTx>, host: Option<ResolvedHost>) -> PushOptions<'static> {
    let mut push_opts = PushOptions::new();
    push_opts.remote_callbacks(make_callbacks(tx, host));
    push_opts
}

/// Components of a git remote URL relevant to ssh_config resolution.
struct GitUrl {
    /// `ssh`, `https`, etc. `None` for scp-like (`git@host:path`).
    scheme: Option<String>,
    user: Option<String>,
    host: String,
    port: Option<u16>,
    /// Everything after the host (and port). For scp-like this is `:path`,
    /// for ssh:// it's `/path` (or empty).
    tail: String,
}

impl GitUrl {
    fn parse(url: &str) -> Option<Self> {
        // ssh://[user@]host[:port]/path
        if let Some(rest) = url.strip_prefix("ssh://") {
            let (authority, tail_no_slash) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, ""),
            };
            let (user, hostport) = match authority.rsplit_once('@') {
                Some((u, h)) => (Some(u.to_string()), h),
                None => (None, authority),
            };
            let (host, port) = match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().ok()),
                None => (hostport.to_string(), None),
            };
            return Some(Self {
                scheme: Some("ssh".to_string()),
                user,
                host,
                port,
                tail: tail_no_slash.to_string(),
            });
        }

        // Other schemes (https, git, file…) we don't rewrite.
        if let Some(idx) = url.find("://") {
            let scheme = url[..idx].to_string();
            // We still return something so callers can detect non-ssh schemes
            // and skip ssh_config resolution.
            return Some(Self {
                scheme: Some(scheme),
                user: None,
                host: String::new(),
                port: None,
                tail: url[idx..].to_string(),
            });
        }

        // scp-like:  [user@]host:path
        if let Some((authority, path)) = url.split_once(':') {
            // Guard against Windows-style `C:\path` and IPv6 — naive but
            // sufficient for git remote URLs we'd plausibly see.
            if authority.contains('/') || authority.is_empty() {
                return None;
            }
            let (user, host) = match authority.rsplit_once('@') {
                Some((u, h)) => (Some(u.to_string()), h.to_string()),
                None => (None, authority.to_string()),
            };
            return Some(Self {
                scheme: None,
                user,
                host,
                port: None,
                tail: format!(":{}", path),
            });
        }

        None
    }

    fn rebuild(&self, hostname: &str, user: Option<&str>, port: Option<u16>) -> String {
        let user = user.or(self.user.as_deref());
        let user_at = user.map(|u| format!("{}@", u)).unwrap_or_default();

        match self.scheme.as_deref() {
            Some("ssh") => {
                let port_part = port
                    .or(self.port)
                    .map(|p| format!(":{}", p))
                    .unwrap_or_default();
                format!("ssh://{}{}{}{}", user_at, hostname, port_part, self.tail)
            }
            None => {
                // scp-like form has no port syntax; if a non-default port was
                // requested via ssh_config, upgrade to ssh:// so libssh2 sees it.
                if let Some(p) = port {
                    format!(
                        "ssh://{}{}:{}{}",
                        user_at,
                        hostname,
                        p,
                        self.tail.replacen(':', "/", 1)
                    )
                } else {
                    format!("{}{}{}", user_at, hostname, self.tail)
                }
            }
            // Non-ssh schemes don't get rewritten.
            _ => self.tail.clone(),
        }
    }

    fn is_ssh(&self) -> bool {
        matches!(self.scheme.as_deref(), Some("ssh") | None) && !self.host.is_empty()
    }
}

/// For an SSH remote URL, return (effective_url, resolved_host_or_none).
/// If ssh_config produced no overrides the URL is returned unchanged.
fn apply_ssh_config(url: &str) -> (String, Option<ResolvedHost>) {
    let Some(parsed) = GitUrl::parse(url) else {
        return (url.to_string(), None);
    };
    if !parsed.is_ssh() {
        return (url.to_string(), None);
    }

    let resolved = ssh_config::resolve(&parsed.host);
    let host_changed = resolved.hostname != parsed.host;
    let user_changed = parsed.user.is_none() && resolved.user.is_some();
    let port_changed = parsed.port.is_none() && resolved.port.is_some();

    let effective = if host_changed || user_changed || port_changed {
        parsed.rebuild(&resolved.hostname, resolved.user.as_deref(), resolved.port)
    } else {
        url.to_string()
    };

    (effective, Some(resolved))
}

pub fn fetch_repo(path: &Path) -> Result<(), AppError> {
    fetch_repo_with_progress(path, None)
}

pub fn fetch_repo_with_progress(path: &Path, tx: Option<ProgressTx>) -> Result<(), AppError> {
    let repo = Repository::open(path)?;

    let remotes = repo.remotes()?;
    for remote_name in remotes.iter().flatten() {
        emit(
            tx.as_ref(),
            ProgressEvent::Stage(format!("fetching {}", remote_name)),
        );
        let remote = repo.find_remote(remote_name)?;

        let refspecs: Vec<String> = remote
            .fetch_refspecs()?
            .iter()
            .flatten()
            .map(String::from)
            .collect();
        let refspec_refs: Vec<&str> = refspecs.iter().map(|s| s.as_str()).collect();

        let original_url = remote.url().unwrap_or("").to_string();
        let (effective_url, resolved) = apply_ssh_config(&original_url);

        if effective_url != original_url {
            emit(
                tx.as_ref(),
                ProgressEvent::Stage(format!(
                    "ssh_config: {} → {}",
                    redact_userinfo(&original_url),
                    redact_userinfo(&effective_url),
                )),
            );
        }

        // Use an anonymous remote with the (possibly rewritten) URL but the
        // configured fetch refspecs — refs still land under refs/remotes/<name>/.
        // The persisted remote config is left untouched.
        let mut anon = repo.remote_anonymous(&effective_url)?;
        anon.fetch(
            &refspec_refs,
            Some(&mut make_fetch_options(tx.clone(), resolved)),
            None,
        )?;
    }

    Ok(())
}

/// Strip `user@` from a URL for display in progress messages.
fn redact_userinfo(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("ssh://") {
        if let Some((_, after_at)) = rest.split_once('@') {
            return format!("ssh://{}", after_at);
        }
        return url.to_string();
    }
    if let Some((_, after_at)) = url.split_once('@') {
        if !url.contains("://") {
            return after_at.to_string();
        }
    }
    url.to_string()
}

pub fn pull_rebase_repo(path: &Path) -> Result<(), AppError> {
    pull_rebase_repo_with_progress(path, None)
}

pub fn pull_rebase_repo_with_progress(path: &Path, tx: Option<ProgressTx>) -> Result<(), AppError> {
    fetch_repo_with_progress(path, tx.clone())?;

    let repo = Repository::open(path)?;

    let head = repo.head()?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| AppError::msg("HEAD is not on a branch"))?
        .to_string();

    let upstream_ref_name = format!("refs/remotes/origin/{}", branch_name);
    let upstream_ref = repo
        .find_reference(&upstream_ref_name)
        .map_err(|_| AppError::msg(format!("No upstream branch found for {}", branch_name)))?;
    let upstream_commit = repo.reference_to_annotated_commit(&upstream_ref)?;

    emit(
        tx.as_ref(),
        ProgressEvent::Stage(format!("rebasing onto origin/{}", branch_name)),
    );
    rebase_onto(&repo, &upstream_commit, tx.as_ref())?;

    Ok(())
}

fn rebase_onto(
    repo: &Repository,
    upstream: &AnnotatedCommit,
    tx: Option<&ProgressTx>,
) -> Result<(), AppError> {
    let mut rebase = repo.rebase(None, Some(upstream), None, None)?;

    let total = rebase.len();
    let mut current = 0usize;
    while let Some(op) = rebase.next() {
        let _op = op?;
        current += 1;
        emit(tx, ProgressEvent::RebaseStep { current, total });
        let index = repo.index()?;
        if index.has_conflicts() {
            rebase.abort()?;
            return Err(AppError::msg("Rebase aborted: conflicts detected"));
        }
        let committer = repo.signature()?;
        rebase.commit(None, &committer, None)?;
    }

    rebase.finish(None)?;

    Ok(())
}

pub fn push_repo(path: &Path) -> Result<(), AppError> {
    push_repo_with_progress(path, None)
}

pub fn push_repo_with_progress(path: &Path, tx: Option<ProgressTx>) -> Result<(), AppError> {
    let repo = Repository::open(path)?;

    let head = repo.head()?;
    let branch_name = head
        .shorthand()
        .ok_or_else(|| AppError::msg("HEAD is not on a branch"))?
        .to_string();

    let remote = repo
        .find_remote("origin")
        .map_err(|_| AppError::msg("No 'origin' remote found"))?;

    let original_url = remote
        .pushurl()
        .or_else(|| remote.url())
        .unwrap_or("")
        .to_string();
    let (effective_url, resolved) = apply_ssh_config(&original_url);

    if effective_url != original_url {
        emit(
            tx.as_ref(),
            ProgressEvent::Stage(format!(
                "ssh_config: {} → {}",
                redact_userinfo(&original_url),
                redact_userinfo(&effective_url),
            )),
        );
    }

    emit(
        tx.as_ref(),
        ProgressEvent::Stage(format!("pushing {} → origin", branch_name)),
    );
    let refspec = format!("refs/heads/{}:refs/heads/{}", branch_name, branch_name);
    let mut anon = repo.remote_anonymous(&effective_url)?;
    anon.push(&[&refspec], Some(&mut make_push_options(tx, resolved)))?;

    Ok(())
}
