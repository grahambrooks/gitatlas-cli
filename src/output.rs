use comfy_table::{presets, Attribute, Cell, Color, ContentArrangement, Table};
use console::style;
use serde::Serialize;

use crate::models::{
    BranchInfo, CommitFileChange, CommitInfo, FileChange, RefKind, RemoteInfo, RepoHealth,
    RepoInfo, StashEntry,
};

pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{}", s),
        Err(e) => eprintln!("Failed to serialize: {}", e),
    }
}

fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}

fn health_cell(h: RepoHealth) -> Cell {
    let (label, color) = match h {
        RepoHealth::Clean => ("clean", Color::Green),
        RepoHealth::Dirty => ("dirty", Color::Yellow),
        RepoHealth::Diverged => ("diverged", Color::Red),
        RepoHealth::Error => ("error", Color::Red),
    };
    Cell::new(label).fg(color).add_attribute(Attribute::Bold)
}

pub fn print_repo_table(repos: &[RepoInfo]) {
    if repos.is_empty() {
        println!("(no repositories)");
        return;
    }

    let mut table = new_table();
    table.set_header(vec![
        Cell::new("NAME").add_attribute(Attribute::Bold),
        Cell::new("BRANCH").add_attribute(Attribute::Bold),
        Cell::new("AHEAD").add_attribute(Attribute::Bold),
        Cell::new("BEHIND").add_attribute(Attribute::Bold),
        Cell::new("DIRTY").add_attribute(Attribute::Bold),
        Cell::new("STASH").add_attribute(Attribute::Bold),
        Cell::new("HEALTH").add_attribute(Attribute::Bold),
        Cell::new("PATH").add_attribute(Attribute::Bold),
    ]);

    for r in repos {
        let ahead = if r.ahead > 0 {
            Cell::new(r.ahead).fg(Color::Yellow)
        } else {
            Cell::new(r.ahead)
        };
        let behind = if r.behind > 0 {
            Cell::new(r.behind).fg(Color::Red)
        } else {
            Cell::new(r.behind)
        };
        let dirty = if r.dirty_files > 0 {
            Cell::new(r.dirty_files).fg(Color::Yellow)
        } else {
            Cell::new(r.dirty_files)
        };
        table.add_row(vec![
            Cell::new(&r.name),
            Cell::new(&r.branch),
            ahead,
            behind,
            dirty,
            Cell::new(r.stash_count),
            health_cell(r.health),
            Cell::new(&r.path),
        ]);
    }
    println!("{table}");

    // Health counts summary
    let mut clean = 0;
    let mut dirty = 0;
    let mut diverged = 0;
    let mut error = 0;
    for r in repos {
        match r.health {
            RepoHealth::Clean => clean += 1,
            RepoHealth::Dirty => dirty += 1,
            RepoHealth::Diverged => diverged += 1,
            RepoHealth::Error => error += 1,
        }
    }
    println!(
        "\n{} total — {} clean, {} dirty, {} diverged, {} error",
        repos.len(),
        style(clean).green(),
        style(dirty).yellow(),
        style(diverged).red(),
        style(error).red(),
    );
}

pub fn print_repo_status(repo: &RepoInfo) {
    println!("{}", style(&repo.name).bold().cyan());
    println!("  path:        {}", repo.path);
    println!("  branch:      {}", repo.branch);
    println!("  ahead:       {}   behind: {}", repo.ahead, repo.behind);
    println!("  dirty files: {}", repo.dirty_files);
    println!("  stashes:     {}", repo.stash_count);
    let health_str = match repo.health {
        RepoHealth::Clean => style("clean").green().to_string(),
        RepoHealth::Dirty => style("dirty").yellow().to_string(),
        RepoHealth::Diverged => style("diverged").red().to_string(),
        RepoHealth::Error => style("error").red().to_string(),
    };
    println!("  health:      {}", health_str);
    if let Some(url) = &repo.remote_url {
        println!("  origin:      {}", url);
    }
    println!("  checked:     {}", repo.last_checked);
}

pub fn print_commits(commits: &[CommitInfo]) {
    if commits.is_empty() {
        println!("(no commits)");
        return;
    }
    for c in commits {
        let refs = if c.refs.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = c
                .refs
                .iter()
                .map(|r| match r.kind {
                    RefKind::Head => style(format!("HEAD -> {}", r.name))
                        .cyan()
                        .bold()
                        .to_string(),
                    RefKind::Local => style(&r.name).green().to_string(),
                    RefKind::Remote => style(&r.name).red().to_string(),
                    RefKind::Tag => style(format!("tag: {}", r.name)).yellow().to_string(),
                })
                .collect();
            format!(" ({})", parts.join(", "))
        };
        println!(
            "{}{} {} {}",
            style(&c.short_oid).yellow(),
            refs,
            style(format!("<{}>", c.author)).dim(),
            c.message.lines().next().unwrap_or(""),
        );
    }
}

pub fn print_file_changes(changes: &[FileChange]) {
    if changes.is_empty() {
        println!("(clean working tree)");
        return;
    }
    let staged: Vec<&FileChange> = changes.iter().filter(|c| c.staged).collect();
    let unstaged: Vec<&FileChange> = changes.iter().filter(|c| !c.staged).collect();

    if !staged.is_empty() {
        println!("{}", style("Staged:").green().bold());
        for c in &staged {
            println!("  {} {}", style(c.status.short()).green(), c.path);
        }
    }
    if !unstaged.is_empty() {
        if !staged.is_empty() {
            println!();
        }
        println!("{}", style("Unstaged:").yellow().bold());
        for c in &unstaged {
            println!("  {} {}", style(c.status.short()).yellow(), c.path);
        }
    }
}

pub fn print_diff(diff: &str) {
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            println!("{}", style(line).bold());
        } else if line.starts_with('+') {
            println!("{}", style(line).green());
        } else if line.starts_with('-') {
            println!("{}", style(line).red());
        } else if line.starts_with("@@") {
            println!("{}", style(line).cyan());
        } else if line.starts_with("diff --git") {
            println!("{}", style(line).bold().bright());
        } else {
            println!("{}", line);
        }
    }
}

pub fn print_branches(branches: &[BranchInfo]) {
    if branches.is_empty() {
        println!("(no branches)");
        return;
    }
    for b in branches {
        let marker = if b.is_head { "*" } else { " " };
        let name = if b.is_head {
            style(&b.name).green().bold().to_string()
        } else if b.is_remote {
            style(&b.name).red().to_string()
        } else {
            b.name.clone()
        };
        let upstream = match &b.upstream {
            Some(u) => style(format!(" → {}", u)).dim().to_string(),
            None => String::new(),
        };
        println!("  {} {}{}", marker, name, upstream);
    }
}

pub fn print_stashes(stashes: &[StashEntry]) {
    if stashes.is_empty() {
        println!("(no stashes)");
        return;
    }
    for s in stashes {
        println!("  stash@{{{}}}: {}", s.index, s.message);
    }
}

pub fn print_remotes(remotes: &[RemoteInfo]) {
    if remotes.is_empty() {
        println!("(no remotes)");
        return;
    }
    let mut table = new_table();
    table.set_header(vec![
        Cell::new("NAME").add_attribute(Attribute::Bold),
        Cell::new("URL").add_attribute(Attribute::Bold),
    ]);
    for r in remotes {
        table.add_row(vec![Cell::new(&r.name), Cell::new(&r.url)]);
    }
    println!("{table}");
}

pub fn print_commit_files(files: &[CommitFileChange]) {
    if files.is_empty() {
        println!("(no file changes)");
        return;
    }
    for f in files {
        let label = match f.status.as_str() {
            "added" => style("A").green().to_string(),
            "deleted" => style("D").red().to_string(),
            "modified" => style("M").yellow().to_string(),
            "renamed" => style("R").cyan().to_string(),
            "copied" => style("C").cyan().to_string(),
            _ => "?".to_string(),
        };
        println!("  {} {}", label, f.path);
    }
}
