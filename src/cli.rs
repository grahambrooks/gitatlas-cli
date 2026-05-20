use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "gitatlas",
    version,
    about = "Multi-repo Git management CLI (companion to the gitatlas GUI)",
    long_about = None,
)]
pub struct Cli {
    /// Emit machine-readable JSON on stdout instead of human-readable output.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scan configured roots for Git repositories and refresh the cache.
    Scan,

    /// List cached repositories (optionally filtered).
    #[command(alias = "ls")]
    List(ListArgs),

    /// Show detailed status for a single repo.
    Status(RepoArg),

    /// Fetch all remotes for one or more repos (default: all cached).
    Fetch(BulkArgs),

    /// Fetch + rebase the current branch for one or more repos.
    Pull(BulkArgs),

    /// Push the current branch to origin.
    Push(RepoArg),

    /// Show the commit log.
    Log(LogArgs),

    /// Show a single commit (diff + file list).
    Show(ShowArgs),

    /// Show a working-tree or staged diff.
    Diff(DiffArgs),

    /// Stage one or more files.
    Add(StageArgs),

    /// Unstage one or more files.
    Reset(StageArgs),

    /// Create a commit from the current index.
    Commit(CommitArgs),

    /// Squash the N most recent commits into one.
    Squash(SquashArgs),

    /// Branch management.
    Branch(BranchCmd),

    /// Stash management.
    Stash(StashCmd),

    /// Remote management.
    Remote(RemoteCmd),

    /// View or update the repo's git user profile.
    Profile(ProfileArgs),

    /// Print the repository's README to stdout.
    Readme(RepoArg),

    /// Print a PR-creation URL for the current branch.
    Pr(RepoArg),

    /// Manage gitatlas configuration (scan roots).
    Config(ConfigCmd),

    /// Launch the interactive terminal UI.
    Tui,
}

#[derive(Debug, Args)]
pub struct RepoArg {
    /// Repo name (from the cache) or path to the repo.
    pub repo: String,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Filter by health status.
    #[arg(long, value_parser = ["clean", "dirty", "diverged", "error"])]
    pub health: Option<String>,

    /// Search in repo name, branch, or path.
    #[arg(long, short = 'q')]
    pub search: Option<String>,

    /// Refresh the cache first (equivalent to running `scan` before `list`).
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct BulkArgs {
    /// Repo name(s) or path(s). Omit with --all to operate on all cached repos.
    pub repos: Vec<String>,

    /// Operate on every cached repo.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct LogArgs {
    pub repo: String,

    /// Limit the number of commits.
    #[arg(short = 'n', long, default_value_t = 50)]
    pub count: usize,

    /// Limit to commits touching a specific file.
    #[arg(long)]
    pub file: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    pub repo: String,
    pub commit: String,
}

#[derive(Debug, Args)]
pub struct DiffArgs {
    pub repo: String,

    /// Show the staged diff instead of working-tree changes.
    #[arg(long)]
    pub staged: bool,

    /// Restrict to a specific path.
    pub path: Option<String>,
}

#[derive(Debug, Args)]
pub struct StageArgs {
    pub repo: String,

    /// File paths (relative to the repo root).
    pub paths: Vec<String>,

    /// Stage/unstage all changes.
    #[arg(long, short = 'A')]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct CommitArgs {
    pub repo: String,

    /// Commit message.
    #[arg(short = 'm', long)]
    pub message: String,
}

#[derive(Debug, Args)]
pub struct SquashArgs {
    pub repo: String,

    /// Number of commits from HEAD to squash.
    #[arg(short = 'n', long)]
    pub count: usize,

    /// Commit message for the squashed commit.
    #[arg(short = 'm', long)]
    pub message: String,
}

// ── Branch ───────────────────────────────

#[derive(Debug, Args)]
pub struct BranchCmd {
    pub repo: String,

    #[command(subcommand)]
    pub action: Option<BranchAction>,
}

#[derive(Debug, Subcommand)]
pub enum BranchAction {
    /// List branches (default).
    List,
    /// Create a new branch at HEAD.
    Create { name: String },
    /// Check out a branch.
    Checkout { name: String },
    /// Delete a local branch.
    Delete { name: String },
    /// Merge a branch into the current branch.
    Merge { name: String },
}

// ── Stash ────────────────────────────────

#[derive(Debug, Args)]
pub struct StashCmd {
    pub repo: String,

    #[command(subcommand)]
    pub action: Option<StashAction>,
}

#[derive(Debug, Subcommand)]
pub enum StashAction {
    /// List stashes (default).
    List,
    /// Save the working tree as a stash.
    Save {
        #[arg(short = 'm', long, default_value = "")]
        message: String,
    },
    /// Pop a stash (default: 0).
    Pop {
        #[arg(default_value_t = 0)]
        index: usize,
    },
    /// Drop a stash.
    Drop { index: usize },
}

// ── Remote ───────────────────────────────

#[derive(Debug, Args)]
pub struct RemoteCmd {
    pub repo: String,

    #[command(subcommand)]
    pub action: Option<RemoteAction>,
}

#[derive(Debug, Subcommand)]
pub enum RemoteAction {
    /// List remotes (default).
    List,
    Add {
        name: String,
        url: String,
    },
    Remove {
        name: String,
    },
    Rename {
        old: String,
        new: String,
    },
}

// ── Profile ──────────────────────────────

#[derive(Debug, Args)]
pub struct ProfileArgs {
    pub repo: String,

    /// Set user.name.
    #[arg(long)]
    pub set_name: Option<String>,

    /// Set user.email.
    #[arg(long)]
    pub set_email: Option<String>,
}

// ── Config ───────────────────────────────

#[derive(Debug, Args)]
pub struct ConfigCmd {
    #[command(subcommand)]
    pub action: ConfigAction,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the current config.
    Show,
    /// Manage scan roots.
    Roots {
        #[command(subcommand)]
        action: RootsAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum RootsAction {
    List,
    Add {
        path: String,
    },
    Remove {
        path: String,
    },
    /// Replace the scan roots with the given paths.
    Set {
        paths: Vec<String>,
    },
}
