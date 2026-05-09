use std::path::Path;

use anyhow::Result;
use console::style;
use serde_json::json;

use crate::cache::{self, Config};
use crate::cli::*;
use crate::error::AppError;
use crate::git::{detail, operations};
use crate::models::{RepoHealth, RepoInfo};
use crate::output;
use crate::resolver;
use crate::scanner;

pub struct Ctx {
    pub json: bool,
}

pub fn dispatch(cli: Cli) -> Result<()> {
    let ctx = Ctx { json: cli.json };

    match cli.command {
        Command::Scan => scan(&ctx),
        Command::List(args) => list(&ctx, args),
        Command::Status(args) => status(&ctx, args),
        Command::Fetch(args) => fetch(&ctx, args),
        Command::Pull(args) => pull(&ctx, args),
        Command::Push(args) => push(&ctx, args),
        Command::Log(args) => log(&ctx, args),
        Command::Show(args) => show(&ctx, args),
        Command::Diff(args) => diff(&ctx, args),
        Command::Add(args) => stage(&ctx, args, true),
        Command::Reset(args) => stage(&ctx, args, false),
        Command::Commit(args) => commit(&ctx, args),
        Command::Squash(args) => squash(&ctx, args),
        Command::Branch(cmd) => branch(&ctx, cmd),
        Command::Stash(cmd) => stash(&ctx, cmd),
        Command::Remote(cmd) => remote(&ctx, cmd),
        Command::Profile(args) => profile(&ctx, args),
        Command::Readme(args) => readme(&ctx, args),
        Command::Pr(args) => pr(&ctx, args),
        Command::Config(cmd) => config_cmd(&ctx, cmd),
        Command::Tui => crate::tui::run().map_err(anyhow::Error::from),
    }
}

// ── Scan / list / status ─────────────────

fn scan(ctx: &Ctx) -> Result<()> {
    let roots = cache::effective_scan_roots();
    if roots.is_empty() {
        return Err(anyhow::anyhow!(
            "No scan roots configured and ~/dev is not present.\n\
             Add one with: gitatlas config roots add <path>"
        ));
    }

    if !ctx.json {
        eprintln!(
            "Scanning {}...",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let repos = scanner::scan_roots(&roots);
    cache::save(&repos);

    if ctx.json {
        output::print_json(&repos);
    } else {
        output::print_repo_table(&repos);
    }
    Ok(())
}

fn list(ctx: &Ctx, args: ListArgs) -> Result<()> {
    if args.refresh {
        return scan(ctx).and_then(|_| {
            let repos = filter_cache(&args);
            emit_list(ctx, &repos);
            Ok(())
        });
    }

    let repos = filter_cache(&args);
    if repos.is_empty() {
        if !ctx.json {
            eprintln!(
                "No cached repos{}. Run `gitatlas scan` first.",
                match &args.search {
                    Some(q) => format!(" matching '{}'", q),
                    None => String::new(),
                }
            );
        }
    }
    emit_list(ctx, &repos);
    Ok(())
}

fn filter_cache(args: &ListArgs) -> Vec<RepoInfo> {
    let mut repos = cache::load();

    if let Some(h) = &args.health {
        let target = RepoHealth::parse(h).unwrap_or(RepoHealth::Error);
        repos.retain(|r| r.health == target);
    }

    if let Some(q) = &args.search {
        let needle = q.to_lowercase();
        repos.retain(|r| {
            r.name.to_lowercase().contains(&needle)
                || r.branch.to_lowercase().contains(&needle)
                || r.path.to_lowercase().contains(&needle)
        });
    }

    repos
}

fn emit_list(ctx: &Ctx, repos: &[RepoInfo]) {
    if ctx.json {
        output::print_json(&repos);
    } else {
        output::print_repo_table(repos);
    }
}

fn status(ctx: &Ctx, args: RepoArg) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let info = crate::git::status::get_repo_info(&path);
    if ctx.json {
        output::print_json(&info);
    } else {
        output::print_repo_status(&info);
    }
    Ok(())
}

// ── Remote ops ───────────────────────────

fn fetch(ctx: &Ctx, args: BulkArgs) -> Result<()> {
    let targets = resolver::resolve_many(&args.repos, args.all)?;
    let mut results = Vec::new();
    for (name, path) in &targets {
        if !ctx.json {
            eprintln!("{} {}", style("fetch").cyan(), name);
        }
        let outcome = operations::fetch_repo(path);
        let updated = crate::git::status::get_repo_info(path);
        results.push(bulk_result(name, path, &outcome, &updated));
        report_outcome(ctx, name, "fetch", &outcome);
    }
    // Refresh cache for touched repos
    refresh_cache_for(&results);
    if ctx.json {
        output::print_json(&results);
    }
    Ok(())
}

fn pull(ctx: &Ctx, args: BulkArgs) -> Result<()> {
    let targets = resolver::resolve_many(&args.repos, args.all)?;
    let mut results = Vec::new();
    for (name, path) in &targets {
        if !ctx.json {
            eprintln!("{} {}", style("pull").cyan(), name);
        }
        let outcome = operations::pull_rebase_repo(path);
        let updated = crate::git::status::get_repo_info(path);
        results.push(bulk_result(name, path, &outcome, &updated));
        report_outcome(ctx, name, "pull", &outcome);
    }
    refresh_cache_for(&results);
    if ctx.json {
        output::print_json(&results);
    }
    Ok(())
}

fn push(ctx: &Ctx, args: RepoArg) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let outcome = operations::push_repo(&path);
    report_outcome(ctx, &args.repo, "push", &outcome);
    // Refresh cached status
    let updated = crate::git::status::get_repo_info(&path);
    let mut cache = cache::load();
    if let Some(existing) = cache.iter_mut().find(|r| r.path == updated.path) {
        *existing = updated.clone();
        cache::save(&cache);
    }
    if ctx.json {
        output::print_json(&serde_json::json!({
            "repo": args.repo,
            "ok": outcome.is_ok(),
            "error": outcome.err().map(|e| e.to_string()),
        }));
    }
    Ok(())
}

fn bulk_result(
    name: &str,
    path: &Path,
    outcome: &Result<(), AppError>,
    updated: &RepoInfo,
) -> serde_json::Value {
    json!({
        "name": name,
        "path": path.display().to_string(),
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err().map(|e| e.to_string()),
        "status": updated,
    })
}

fn report_outcome(ctx: &Ctx, name: &str, op: &str, outcome: &Result<(), AppError>) {
    if ctx.json {
        return;
    }
    match outcome {
        Ok(_) => println!("  {} {} {}", style("✓").green(), op, name),
        Err(e) => println!("  {} {} {}: {}", style("✗").red(), op, name, e),
    }
}

fn refresh_cache_for(results: &[serde_json::Value]) {
    let mut cache = cache::load();
    for r in results {
        let Some(path) = r.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(status) = r.get("status") else { continue };
        let Ok(updated) = serde_json::from_value::<RepoInfo>(status.clone()) else {
            continue;
        };
        if let Some(existing) = cache.iter_mut().find(|c| c.path == path) {
            *existing = updated;
        } else {
            cache.push(updated);
        }
    }
    cache::save(&cache);
}

// ── Log / show / diff ────────────────────

fn log(ctx: &Ctx, args: LogArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let commits = match args.file {
        Some(ref f) => detail::get_file_history(&path, f, args.count)?,
        None => detail::get_commit_log(&path, args.count)?,
    };
    if ctx.json {
        output::print_json(&commits);
    } else {
        output::print_commits(&commits);
    }
    Ok(())
}

fn show(ctx: &Ctx, args: ShowArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let files = detail::get_commit_files(&path, &args.commit)?;
    let diff = detail::get_commit_diff(&path, &args.commit)?;

    if ctx.json {
        output::print_json(&json!({
            "commit": args.commit,
            "files": files,
            "diff": diff,
        }));
    } else {
        println!("{} {}", style("commit").yellow(), args.commit);
        println!();
        output::print_commit_files(&files);
        println!();
        output::print_diff(&diff);
    }
    Ok(())
}

fn diff(ctx: &Ctx, args: DiffArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let text = match args.path {
        Some(p) => detail::get_file_diff(&path, &p, args.staged)?,
        None => detail::get_workdir_diff(&path, args.staged)?,
    };
    if ctx.json {
        output::print_json(&json!({ "diff": text }));
    } else if text.is_empty() {
        println!("(no changes)");
    } else {
        output::print_diff(&text);
    }
    Ok(())
}

// ── Staging / commit / squash ────────────

fn stage(ctx: &Ctx, args: StageArgs, add: bool) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;

    if args.all {
        if add {
            detail::stage_all(&path)?;
        } else {
            detail::unstage_all(&path)?;
        }
    } else {
        if args.paths.is_empty() {
            return Err(anyhow::anyhow!("Specify file paths, or pass --all"));
        }
        if add {
            detail::stage_files(&path, &args.paths)?;
        } else {
            detail::unstage_files(&path, &args.paths)?;
        }
    }

    // Show resulting state
    let changes = detail::get_file_changes(&path)?;
    if ctx.json {
        output::print_json(&changes);
    } else {
        output::print_file_changes(&changes);
    }
    Ok(())
}

fn commit(ctx: &Ctx, args: CommitArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let oid = detail::create_commit(&path, &args.message)?;
    if ctx.json {
        output::print_json(&json!({ "oid": oid }));
    } else {
        println!("{} {}", style("commit").green().bold(), &oid[..7]);
        println!("  {}", args.message);
    }
    Ok(())
}

fn squash(ctx: &Ctx, args: SquashArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let oid = detail::squash_commits(&path, args.count, &args.message)?;
    if ctx.json {
        output::print_json(&json!({ "oid": oid }));
    } else {
        println!(
            "{} {} commits into {}",
            style("squashed").green().bold(),
            args.count,
            &oid[..7]
        );
    }
    Ok(())
}

// ── Branch ───────────────────────────────

fn branch(ctx: &Ctx, cmd: BranchCmd) -> Result<()> {
    let path = resolver::resolve(&cmd.repo)?;
    let action = cmd.action.unwrap_or(BranchAction::List);

    match action {
        BranchAction::List => {
            let branches = detail::get_branches(&path)?;
            if ctx.json {
                output::print_json(&branches);
            } else {
                output::print_branches(&branches);
            }
        }
        BranchAction::Create { name } => {
            detail::create_branch(&path, &name)?;
            println!("{} {}", style("created branch").green(), name);
        }
        BranchAction::Checkout { name } => {
            detail::checkout_branch(&path, &name)?;
            println!("{} {}", style("checked out").green(), name);
        }
        BranchAction::Delete { name } => {
            detail::delete_branch(&path, &name)?;
            println!("{} {}", style("deleted").green(), name);
        }
        BranchAction::Merge { name } => {
            let result = detail::merge_branch(&path, &name)?;
            println!("{} {}: {}", style("merge").green(), name, result);
        }
    }
    Ok(())
}

// ── Stash ────────────────────────────────

fn stash(ctx: &Ctx, cmd: StashCmd) -> Result<()> {
    let path = resolver::resolve(&cmd.repo)?;
    let action = cmd.action.unwrap_or(StashAction::List);

    match action {
        StashAction::List => {
            let stashes = detail::get_stashes(&path)?;
            if ctx.json {
                output::print_json(&stashes);
            } else {
                output::print_stashes(&stashes);
            }
        }
        StashAction::Save { message } => {
            detail::stash_save(&path, &message)?;
            println!("{}", style("stash saved").green());
        }
        StashAction::Pop { index } => {
            detail::stash_pop(&path, index)?;
            println!("{} stash@{{{}}}", style("popped").green(), index);
        }
        StashAction::Drop { index } => {
            detail::stash_drop(&path, index)?;
            println!("{} stash@{{{}}}", style("dropped").green(), index);
        }
    }
    Ok(())
}

// ── Remote ───────────────────────────────

fn remote(ctx: &Ctx, cmd: RemoteCmd) -> Result<()> {
    let path = resolver::resolve(&cmd.repo)?;
    let action = cmd.action.unwrap_or(RemoteAction::List);

    match action {
        RemoteAction::List => {
            let remotes = detail::get_remotes(&path)?;
            if ctx.json {
                output::print_json(&remotes);
            } else {
                output::print_remotes(&remotes);
            }
        }
        RemoteAction::Add { name, url } => {
            detail::add_remote(&path, &name, &url)?;
            println!("{} {} -> {}", style("added remote").green(), name, url);
        }
        RemoteAction::Remove { name } => {
            detail::remove_remote(&path, &name)?;
            println!("{} {}", style("removed remote").green(), name);
        }
        RemoteAction::Rename { old, new } => {
            detail::rename_remote(&path, &old, &new)?;
            println!("{} {} -> {}", style("renamed remote").green(), old, new);
        }
    }
    Ok(())
}

// ── Profile ──────────────────────────────

fn profile(ctx: &Ctx, args: ProfileArgs) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;

    if args.set_name.is_some() || args.set_email.is_some() {
        let current = detail::get_git_profile(&path).unwrap_or_default_profile();
        let new_name = args.set_name.as_deref().unwrap_or(&current.name);
        let new_email = args.set_email.as_deref().unwrap_or(&current.email);
        detail::set_git_profile(&path, new_name, new_email)?;
    }

    let profile = detail::get_git_profile(&path)?;
    if ctx.json {
        output::print_json(&profile);
    } else {
        println!("  user.name:  {}", profile.name);
        println!("  user.email: {}", profile.email);
    }
    Ok(())
}

trait ProfileOrDefault {
    fn unwrap_or_default_profile(self) -> crate::models::GitProfile;
}

impl ProfileOrDefault for Result<crate::models::GitProfile, AppError> {
    fn unwrap_or_default_profile(self) -> crate::models::GitProfile {
        self.unwrap_or(crate::models::GitProfile {
            name: String::new(),
            email: String::new(),
        })
    }
}

// ── Readme / PR ──────────────────────────

fn readme(ctx: &Ctx, args: RepoArg) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let content = detail::get_readme(&path)?;
    match (content, ctx.json) {
        (Some(c), true) => output::print_json(&json!({ "readme": c })),
        (Some(c), false) => print!("{}", c),
        (None, true) => output::print_json(&json!({ "readme": null })),
        (None, false) => println!("(no README found)"),
    }
    Ok(())
}

fn pr(ctx: &Ctx, args: RepoArg) -> Result<()> {
    let path = resolver::resolve(&args.repo)?;
    let url = detail::get_pr_url(&path)?;
    if ctx.json {
        output::print_json(&json!({ "url": url }));
    } else {
        println!("{}", url);
    }
    Ok(())
}

// ── Config ───────────────────────────────

fn config_cmd(ctx: &Ctx, cmd: ConfigCmd) -> Result<()> {
    match cmd.action {
        ConfigAction::Show => {
            let cfg = cache::load_config();
            if ctx.json {
                output::print_json(&cfg);
            } else {
                let roots = if cfg.scan_roots.is_empty() {
                    "(none; defaulting to ~/dev)".to_string()
                } else {
                    cfg.scan_roots.join("\n  ")
                };
                println!("scan roots:\n  {}", roots);
            }
        }
        ConfigAction::Roots { action } => match action {
            RootsAction::List => {
                let cfg = cache::load_config();
                if ctx.json {
                    output::print_json(&cfg.scan_roots);
                } else if cfg.scan_roots.is_empty() {
                    println!("(no scan roots configured)");
                } else {
                    for r in &cfg.scan_roots {
                        println!("{}", r);
                    }
                }
            }
            RootsAction::Add { path } => {
                let mut cfg = cache::load_config();
                let canonical = Path::new(&path)
                    .canonicalize()
                    .map(|p| p.display().to_string())
                    .unwrap_or(path);
                if !cfg.scan_roots.iter().any(|r| r == &canonical) {
                    cfg.scan_roots.push(canonical.clone());
                    cache::save_config(&cfg);
                }
                if !ctx.json {
                    println!("{} {}", style("added root").green(), canonical);
                }
            }
            RootsAction::Remove { path } => {
                let mut cfg = cache::load_config();
                let before = cfg.scan_roots.len();
                cfg.scan_roots.retain(|r| r != &path);
                cache::save_config(&cfg);
                if !ctx.json {
                    if cfg.scan_roots.len() < before {
                        println!("{} {}", style("removed root").green(), path);
                    } else {
                        println!("(not found) {}", path);
                    }
                }
            }
            RootsAction::Set { paths } => {
                let cfg = Config { scan_roots: paths };
                cache::save_config(&cfg);
                if !ctx.json {
                    println!("{}", style("scan roots updated").green());
                }
            }
        },
    }
    Ok(())
}
