use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use rayon::prelude::*;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::{Frame, Terminal};

use crate::cache;
use crate::git::{detail, operations, status as git_status};
use crate::models::{
    BranchInfo, CommitInfo, FileChange, RefKind, RepoHealth, RepoInfo, StashEntry,
};
use crate::scanner;

pub fn run() -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let res = event_loop(&mut terminal);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;

    res
}

// ── State ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Dashboard,
    Detail,
    Search,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Changes,
    History,
    Branches,
    Stashes,
    Readme,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Changes, Tab::History, Tab::Branches, Tab::Stashes, Tab::Readme];
    fn title(&self) -> &'static str {
        match self {
            Tab::Changes => "Changes",
            Tab::History => "History",
            Tab::Branches => "Branches",
            Tab::Stashes => "Stashes",
            Tab::Readme => "README",
        }
    }
    fn index(&self) -> usize {
        Self::ALL.iter().position(|t| t == self).unwrap_or(0)
    }
    fn next(&self) -> Tab {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }
    fn prev(&self) -> Tab {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

struct Detail {
    repo_index: usize,
    tab: Tab,
    changes: Option<Vec<FileChange>>,
    commits: Option<Vec<CommitInfo>>,
    branches: Option<Vec<BranchInfo>>,
    stashes: Option<Vec<StashEntry>>,
    readme: Option<String>,
    changes_sel: TableState,
    commits_sel: TableState,
    branches_sel: TableState,
    stashes_sel: TableState,
    readme_scroll: u16,
}

impl Detail {
    fn new(repo_index: usize) -> Self {
        Self {
            repo_index,
            tab: Tab::Changes,
            changes: None,
            commits: None,
            branches: None,
            stashes: None,
            readme: None,
            changes_sel: TableState::default().with_selected(0),
            commits_sel: TableState::default().with_selected(0),
            branches_sel: TableState::default().with_selected(0),
            stashes_sel: TableState::default().with_selected(0),
            readme_scroll: 0,
        }
    }
}

enum BulkOp {
    Fetch,
    Pull,
}

impl BulkOp {
    fn verb(&self) -> &'static str {
        match self {
            BulkOp::Fetch => "fetch",
            BulkOp::Pull => "pull",
        }
    }
}

enum WorkerMsg {
    Progress { name: String },
    Done {
        path: String,
        name: String,
        error: Option<String>,
        updated: RepoInfo,
    },
    Finished,
}

struct BulkState {
    op: BulkOp,
    total: usize,
    done: usize,
    current: Option<String>,
    errors: Vec<(String, String)>,
    rx: Receiver<WorkerMsg>,
}

#[derive(Debug, Clone, Copy)]
enum Level {
    Info,
    Success,
    Warn,
    Error,
}

struct StatusMsg {
    text: String,
    level: Level,
    expires: Instant,
}

struct App {
    repos: Vec<RepoInfo>,
    filter_health: Option<RepoHealth>,
    search: String,
    dash_state: TableState,
    mode: Mode,
    prev_mode: Mode,
    detail: Option<Detail>,
    status: Option<StatusMsg>,
    bulk: Option<BulkState>,
    should_quit: bool,
}

impl App {
    fn new() -> Self {
        let repos = cache::load();
        let mut state = TableState::default();
        if !repos.is_empty() {
            state.select(Some(0));
        }
        Self {
            repos,
            filter_health: None,
            search: String::new(),
            dash_state: state,
            mode: Mode::Dashboard,
            prev_mode: Mode::Dashboard,
            detail: None,
            status: None,
            bulk: None,
            should_quit: false,
        }
    }

    fn filtered(&self) -> Vec<usize> {
        let needle = self.search.to_lowercase();
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                if let Some(h) = self.filter_health {
                    if r.health != h {
                        return false;
                    }
                }
                if needle.is_empty() {
                    return true;
                }
                r.name.to_lowercase().contains(&needle)
                    || r.branch.to_lowercase().contains(&needle)
                    || r.path.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn set_status(&mut self, text: impl Into<String>, level: Level) {
        self.status = Some(StatusMsg {
            text: text.into(),
            level,
            expires: Instant::now() + Duration::from_secs(4),
        });
    }

    fn tick(&mut self) {
        if let Some(msg) = &self.status {
            if msg.expires <= Instant::now() {
                self.status = None;
            }
        }
    }

    fn drain_worker(&mut self) {
        let Some(bulk) = self.bulk.as_mut() else { return };
        loop {
            match bulk.rx.try_recv() {
                Ok(WorkerMsg::Progress { name }) => {
                    bulk.current = Some(name);
                }
                Ok(WorkerMsg::Done {
                    path,
                    name,
                    error,
                    updated,
                }) => {
                    bulk.done += 1;
                    if let Some(e) = &error {
                        bulk.errors.push((name.clone(), e.clone()));
                    }
                    // Merge the updated repo info back into the repos list.
                    if let Some(existing) = self.repos.iter_mut().find(|r| r.path == path) {
                        *existing = updated;
                    }
                }
                Ok(WorkerMsg::Finished) => {
                    let errors = bulk.errors.len();
                    let total = bulk.total;
                    let verb = bulk.op.verb();
                    self.bulk = None;
                    // Persist the refreshed cache.
                    cache::save(&self.repos);
                    if errors == 0 {
                        self.set_status(
                            format!("{} all complete ({} repos)", verb, total),
                            Level::Success,
                        );
                    } else {
                        self.set_status(
                            format!("{} finished — {}/{} failed", verb, errors, total),
                            Level::Warn,
                        );
                    }
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Worker died unexpectedly.
                    self.bulk = None;
                    self.set_status("bulk worker disconnected", Level::Error);
                    return;
                }
            }
        }
    }
}

// ── Event loop ────────────────────────────────────────

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> anyhow::Result<()> {
    let mut app = App::new();
    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key);
                }
            }
        }

        app.drain_worker();
        app.tick();

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

// ── Rendering ─────────────────────────────────────────

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, chunks[0], app);
    match app.mode {
        Mode::Dashboard | Mode::Search => draw_dashboard(f, chunks[1], app),
        Mode::Detail => draw_detail(f, chunks[1], app),
        Mode::Help => {
            // Draw dashboard behind and overlay
            draw_dashboard(f, chunks[1], app);
            draw_help(f, area);
        }
    }
    draw_statusline(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let title = Line::from(vec![
        Span::styled(
            " GitAtlas ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        match app.mode {
            Mode::Dashboard => Span::raw("dashboard"),
            Mode::Search => Span::styled(
                format!("search: {}_", app.search),
                Style::default().fg(Color::Yellow),
            ),
            Mode::Detail => {
                let name = app
                    .detail
                    .as_ref()
                    .and_then(|d| app.repos.get(d.repo_index))
                    .map(|r| r.name.as_str())
                    .unwrap_or("");
                Span::styled(format!("repo: {}", name), Style::default().fg(Color::Cyan))
            }
            Mode::Help => Span::raw("help"),
        },
    ]);

    let filter = match app.filter_health {
        None => String::from("all"),
        Some(h) => format!("filter:{}", match h {
            RepoHealth::Clean => "clean",
            RepoHealth::Dirty => "dirty",
            RepoHealth::Diverged => "diverged",
            RepoHealth::Error => "error",
        }),
    };

    let right = Line::from(vec![
        Span::styled(filter, Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(
            format!("{} repos", app.filtered().len()),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw("  "),
        Span::styled("? help", Style::default().fg(Color::DarkGray)),
    ]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(40)])
        .split(area);

    f.render_widget(Paragraph::new(title), cols[0]);
    f.render_widget(
        Paragraph::new(right).alignment(Alignment::Right),
        cols[1],
    );
}

fn draw_statusline(f: &mut Frame, area: Rect, app: &App) {
    if let Some(bulk) = &app.bulk {
        let label = format!(
            " {} {}/{} {}",
            bulk.op.verb(),
            bulk.done,
            bulk.total,
            bulk.current.as_deref().unwrap_or(""),
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                label,
                Style::default().fg(Color::Black).bg(Color::Yellow),
            )),
            area,
        );
        return;
    }

    if let Some(msg) = &app.status {
        let color = match msg.level {
            Level::Info => Color::Blue,
            Level::Success => Color::Green,
            Level::Warn => Color::Yellow,
            Level::Error => Color::Red,
        };
        f.render_widget(
            Paragraph::new(format!(" {}", msg.text)).style(Style::default().fg(color)),
            area,
        );
        return;
    }

    let hint = match app.mode {
        Mode::Dashboard => {
            " ↑/↓ move  enter open  /search  a/c/d/v/e filter  f fetch  p pull  P push  F/U bulk  s scan  q quit"
        }
        Mode::Search => " type to filter   enter apply   esc cancel",
        Mode::Detail => {
            " ←/→ tab  ↑/↓ move  f fetch  p pull  P push  R reload  esc back  q quit"
        }
        Mode::Help => " esc/? close help",
    };
    f.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_dashboard(f: &mut Frame, area: Rect, app: &mut App) {
    let indices = app.filtered();
    // Keep selection in range
    let sel = app.dash_state.selected().unwrap_or(0);
    if !indices.is_empty() && sel >= indices.len() {
        app.dash_state.select(Some(indices.len() - 1));
    } else if indices.is_empty() {
        app.dash_state.select(None);
    } else if app.dash_state.selected().is_none() {
        app.dash_state.select(Some(0));
    }

    let rows: Vec<Row> = indices
        .iter()
        .map(|&i| {
            let r = &app.repos[i];
            Row::new(vec![
                r.name.clone(),
                r.branch.clone(),
                num_cell(r.ahead),
                num_cell(r.behind),
                num_cell(r.dirty_files),
                r.stash_count.to_string(),
                health_label(r.health),
                r.path.clone(),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(28),
        Constraint::Length(22),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Min(20),
    ];

    let header = Row::new(vec![
        "NAME", "BRANCH", "AHEAD", "BEHIND", "DIRTY", "STASH", "HEALTH", "PATH",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan));

    let border_label = if let Some(h) = app.filter_health {
        format!(" repositories (filter: {}) ", health_name(h))
    } else {
        " repositories ".to_string()
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(border_label))
        .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut app.dash_state);
}

fn draw_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(detail) = app.detail.as_mut() else {
        return;
    };
    let Some(repo) = app.repos.get(detail.repo_index).cloned() else {
        return;
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    // Summary block
    let summary_lines = vec![
        Line::from(vec![
            Span::styled(&repo.name, Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
            Span::raw("  "),
            Span::styled(format!("on {}", repo.branch), Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(
                format!("↑{} ↓{}  dirty {}  stash {}  {}",
                    repo.ahead, repo.behind, repo.dirty_files, repo.stash_count,
                    health_name(repo.health)),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("path:   ", Style::default().fg(Color::DarkGray)),
            Span::raw(&repo.path),
        ]),
    ];
    f.render_widget(
        Paragraph::new(summary_lines).block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );

    // Tabs
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(t.title()))
        .collect();
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL))
        .select(detail.tab.index())
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider("│");
    f.render_widget(tabs, chunks[1]);

    // Body by tab
    match detail.tab {
        Tab::Changes => draw_tab_changes(f, chunks[2], detail),
        Tab::History => draw_tab_history(f, chunks[2], detail),
        Tab::Branches => draw_tab_branches(f, chunks[2], detail),
        Tab::Stashes => draw_tab_stashes(f, chunks[2], detail),
        Tab::Readme => draw_tab_readme(f, chunks[2], detail),
    }
}

fn draw_tab_changes(f: &mut Frame, area: Rect, detail: &mut Detail) {
    let rows: Vec<Row> = match &detail.changes {
        Some(changes) if !changes.is_empty() => changes
            .iter()
            .map(|c| {
                let staged_label = if c.staged { "staged" } else { "unstaged" };
                let status_cell = c.status.short().to_string();
                Row::new(vec![
                    staged_label.to_string(),
                    status_cell,
                    c.path.clone(),
                ])
            })
            .collect(),
        Some(_) => {
            f.render_widget(
                Paragraph::new("(clean working tree)").block(Block::default().borders(Borders::ALL).title(" changes ")),
                area,
            );
            return;
        }
        None => {
            f.render_widget(
                Paragraph::new("loading…").block(Block::default().borders(Borders::ALL).title(" changes ")),
                area,
            );
            return;
        }
    };

    let table = Table::new(
        rows,
        [Constraint::Length(10), Constraint::Length(4), Constraint::Min(10)],
    )
    .header(Row::new(vec!["STAGED", "ST", "PATH"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
    .block(Block::default().borders(Borders::ALL).title(" changes "))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut detail.changes_sel);
}

fn draw_tab_history(f: &mut Frame, area: Rect, detail: &mut Detail) {
    let commits = match &detail.commits {
        Some(c) => c,
        None => {
            f.render_widget(
                Paragraph::new("loading…").block(Block::default().borders(Borders::ALL).title(" history ")),
                area,
            );
            return;
        }
    };

    if commits.is_empty() {
        f.render_widget(
            Paragraph::new("(no commits)").block(Block::default().borders(Borders::ALL).title(" history ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = commits
        .iter()
        .map(|c| {
            let refs = c
                .refs
                .iter()
                .map(|r| match r.kind {
                    RefKind::Head => format!("HEAD→{}", r.name),
                    RefKind::Local => r.name.clone(),
                    RefKind::Remote => format!("r/{}", r.name),
                    RefKind::Tag => format!("tag:{}", r.name),
                })
                .collect::<Vec<_>>()
                .join(",");
            let msg = c.message.lines().next().unwrap_or("").to_string();
            let date = c.date.split('T').next().unwrap_or(&c.date).to_string();
            Row::new(vec![
                c.short_oid.clone(),
                date,
                c.author.clone(),
                refs,
                msg,
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(20),
            Constraint::Length(24),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(vec!["SHA", "DATE", "AUTHOR", "REFS", "MESSAGE"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
    .block(Block::default().borders(Borders::ALL).title(" history "))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut detail.commits_sel);
}

fn draw_tab_branches(f: &mut Frame, area: Rect, detail: &mut Detail) {
    let branches = match &detail.branches {
        Some(b) => b,
        None => {
            f.render_widget(
                Paragraph::new("loading…").block(Block::default().borders(Borders::ALL).title(" branches ")),
                area,
            );
            return;
        }
    };

    if branches.is_empty() {
        f.render_widget(
            Paragraph::new("(no branches)").block(Block::default().borders(Borders::ALL).title(" branches ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = branches
        .iter()
        .map(|b| {
            let kind = if b.is_head {
                "HEAD"
            } else if b.is_remote {
                "remote"
            } else {
                "local"
            };
            Row::new(vec![
                if b.is_head { "*".to_string() } else { " ".to_string() },
                kind.to_string(),
                b.name.clone(),
                b.upstream.clone().unwrap_or_default(),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Min(20),
            Constraint::Min(20),
        ],
    )
    .header(Row::new(vec!["", "KIND", "NAME", "UPSTREAM"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
    .block(Block::default().borders(Borders::ALL).title(" branches "))
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut detail.branches_sel);
}

fn draw_tab_stashes(f: &mut Frame, area: Rect, detail: &mut Detail) {
    let stashes = match &detail.stashes {
        Some(s) => s,
        None => {
            f.render_widget(
                Paragraph::new("loading…").block(Block::default().borders(Borders::ALL).title(" stashes ")),
                area,
            );
            return;
        }
    };

    if stashes.is_empty() {
        f.render_widget(
            Paragraph::new("(no stashes)").block(Block::default().borders(Borders::ALL).title(" stashes ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = stashes
        .iter()
        .map(|s| Row::new(vec![format!("stash@{{{}}}", s.index), s.message.clone()]))
        .collect();

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Min(20)])
        .header(Row::new(vec!["REF", "MESSAGE"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
        .block(Block::default().borders(Borders::ALL).title(" stashes "))
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("▶ ");

    f.render_stateful_widget(table, area, &mut detail.stashes_sel);
}

fn draw_tab_readme(f: &mut Frame, area: Rect, detail: &mut Detail) {
    let body = match &detail.readme {
        Some(text) if !text.is_empty() => text.clone(),
        Some(_) => "(README is empty)".to_string(),
        None => "loading…".to_string(),
    };

    let paragraph = Paragraph::new(Text::raw(body))
        .block(Block::default().borders(Borders::ALL).title(" README "))
        .wrap(Wrap { trim: false })
        .scroll((detail.readme_scroll, 0));

    f.render_widget(paragraph, area);
}

fn draw_help(f: &mut Frame, full: Rect) {
    let area = centered(60, 24, full);
    f.render_widget(Clear, area);

    let lines = vec![
        Line::from(Span::styled(
            " GitAtlas — keybindings ",
            Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
        )),
        Line::raw(""),
        Line::from(Span::styled("Dashboard", Style::default().add_modifier(Modifier::BOLD))),
        Line::raw("  ↑/k, ↓/j       move selection"),
        Line::raw("  g / G          jump to top / bottom"),
        Line::raw("  /              incremental search"),
        Line::raw("  a              clear health filter (all)"),
        Line::raw("  c/d/v/e        filter clean / dirty / diverged / error"),
        Line::raw("  enter          open repo detail"),
        Line::raw("  f / p / P      fetch / pull (rebase) / push selected"),
        Line::raw("  F              fetch all repos (background)"),
        Line::raw("  U              pull --rebase all repos (background)"),
        Line::raw("  s              scan configured roots"),
        Line::raw("  R              refresh selected repo status"),
        Line::raw(""),
        Line::from(Span::styled("Detail", Style::default().add_modifier(Modifier::BOLD))),
        Line::raw("  ←/h, →/l       previous / next tab"),
        Line::raw("  1–5            jump to tab"),
        Line::raw("  ↑/k, ↓/j       move selection (or scroll README)"),
        Line::raw("  f / p / P      fetch / pull / push this repo"),
        Line::raw("  R              reload current tab"),
        Line::raw("  esc            back to dashboard"),
        Line::raw(""),
        Line::raw("  q              quit   ?  toggle help"),
    ];

    let help = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" help "))
        .style(Style::default().bg(Color::Black));
    f.render_widget(help, area);
}

fn centered(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(4));
    let h = height.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

fn num_cell(n: u32) -> String {
    n.to_string()
}

fn health_name(h: RepoHealth) -> &'static str {
    match h {
        RepoHealth::Clean => "clean",
        RepoHealth::Dirty => "dirty",
        RepoHealth::Diverged => "diverged",
        RepoHealth::Error => "error",
    }
}

fn health_label(h: RepoHealth) -> String {
    health_name(h).to_string()
}

// ── Key handling ──────────────────────────────────────

fn handle_key(app: &mut App, key: KeyEvent) {
    // Ctrl+C quits from anywhere.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    match app.mode {
        Mode::Search => handle_search_key(app, key),
        Mode::Help => handle_help_key(app, key),
        Mode::Dashboard => handle_dashboard_key(app, key),
        Mode::Detail => handle_detail_key(app, key),
    }
}

fn handle_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.search.clear();
            app.mode = app.prev_mode;
        }
        KeyCode::Enter => {
            app.mode = app.prev_mode;
        }
        KeyCode::Backspace => {
            app.search.pop();
        }
        KeyCode::Char(c) => {
            app.search.push(c);
        }
        _ => {}
    }
}

fn handle_help_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
            app.mode = app.prev_mode;
        }
        _ => {}
    }
}

fn handle_dashboard_key(app: &mut App, key: KeyEvent) {
    let indices = app.filtered();

    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('?') => {
            app.prev_mode = Mode::Dashboard;
            app.mode = Mode::Help;
        }
        KeyCode::Char('/') => {
            app.prev_mode = Mode::Dashboard;
            app.search.clear();
            app.mode = Mode::Search;
        }
        KeyCode::Char('a') => app.filter_health = None,
        KeyCode::Char('c') => app.filter_health = Some(RepoHealth::Clean),
        KeyCode::Char('d') => app.filter_health = Some(RepoHealth::Dirty),
        KeyCode::Char('v') => app.filter_health = Some(RepoHealth::Diverged),
        KeyCode::Char('e') => app.filter_health = Some(RepoHealth::Error),

        KeyCode::Up | KeyCode::Char('k') => move_sel(&mut app.dash_state, indices.len(), -1),
        KeyCode::Down | KeyCode::Char('j') => move_sel(&mut app.dash_state, indices.len(), 1),
        KeyCode::PageUp => move_sel(&mut app.dash_state, indices.len(), -10),
        KeyCode::PageDown => move_sel(&mut app.dash_state, indices.len(), 10),
        KeyCode::Char('g') => {
            if !indices.is_empty() {
                app.dash_state.select(Some(0));
            }
        }
        KeyCode::Char('G') => {
            if !indices.is_empty() {
                app.dash_state.select(Some(indices.len() - 1));
            }
        }

        KeyCode::Enter => {
            if let Some(sel) = app.dash_state.selected() {
                if let Some(&idx) = indices.get(sel) {
                    app.detail = Some(Detail::new(idx));
                    load_current_tab(app);
                    app.mode = Mode::Detail;
                }
            }
        }

        KeyCode::Char('f') => {
            if let Some(path) = selected_path(app, &indices) {
                run_single_sync(app, path, |p| operations::fetch_repo(p), "fetched");
            }
        }
        KeyCode::Char('p') => {
            if let Some(path) = selected_path(app, &indices) {
                run_single_sync(app, path, |p| operations::pull_rebase_repo(p), "pulled");
            }
        }
        KeyCode::Char('P') => {
            if let Some(path) = selected_path(app, &indices) {
                run_single_sync(app, path, |p| operations::push_repo(p), "pushed");
            }
        }

        KeyCode::Char('F') => start_bulk(app, BulkOp::Fetch),
        KeyCode::Char('U') => start_bulk(app, BulkOp::Pull),

        KeyCode::Char('R') => {
            if let Some(path) = selected_path(app, &indices) {
                let info = git_status::get_repo_info(&path);
                if let Some(existing) = app.repos.iter_mut().find(|r| r.path == info.path) {
                    *existing = info;
                }
                cache::save(&app.repos);
                app.set_status("status refreshed", Level::Info);
            }
        }
        KeyCode::Char('s') => run_scan(app),

        _ => {}
    }
}

fn handle_detail_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.mode = Mode::Dashboard;
            return;
        }
        KeyCode::Char('q') => {
            app.should_quit = true;
            return;
        }
        KeyCode::Char('?') => {
            app.prev_mode = Mode::Detail;
            app.mode = Mode::Help;
            return;
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(d) = app.detail.as_mut() {
                d.tab = d.tab.next();
            }
            load_current_tab(app);
            return;
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if let Some(d) = app.detail.as_mut() {
                d.tab = d.tab.prev();
            }
            load_current_tab(app);
            return;
        }
        KeyCode::Char(c @ '1'..='5') => {
            let idx = (c as u8 - b'1') as usize;
            if let Some(d) = app.detail.as_mut() {
                d.tab = Tab::ALL[idx];
            }
            load_current_tab(app);
            return;
        }
        KeyCode::Char('R') => {
            invalidate_current_tab(app);
            load_current_tab(app);
            return;
        }
        _ => {}
    }

    // Tab-specific navigation
    let Some(detail) = app.detail.as_mut() else { return };

    match detail.tab {
        Tab::Changes => nav_table(key.code, &mut detail.changes_sel, list_len(&detail.changes)),
        Tab::History => nav_table(key.code, &mut detail.commits_sel, list_len(&detail.commits)),
        Tab::Branches => nav_table(key.code, &mut detail.branches_sel, list_len(&detail.branches)),
        Tab::Stashes => nav_table(key.code, &mut detail.stashes_sel, list_len(&detail.stashes)),
        Tab::Readme => {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    detail.readme_scroll = detail.readme_scroll.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    detail.readme_scroll = detail.readme_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    detail.readme_scroll = detail.readme_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    detail.readme_scroll = detail.readme_scroll.saturating_add(10);
                }
                KeyCode::Char('g') => detail.readme_scroll = 0,
                _ => {}
            }
        }
    }

    // Detail-level remote ops on current repo
    if let Some(repo) = app.repos.get(app.detail.as_ref().unwrap().repo_index).cloned() {
        let path = PathBuf::from(&repo.path);
        match key.code {
            KeyCode::Char('f') => run_single_sync(app, path, |p| operations::fetch_repo(p), "fetched"),
            KeyCode::Char('p') => run_single_sync(app, path, |p| operations::pull_rebase_repo(p), "pulled"),
            KeyCode::Char('P') => run_single_sync(app, path, |p| operations::push_repo(p), "pushed"),
            _ => {}
        }
    }
}

fn nav_table(key: KeyCode, state: &mut TableState, len: usize) {
    match key {
        KeyCode::Up | KeyCode::Char('k') => move_sel(state, len, -1),
        KeyCode::Down | KeyCode::Char('j') => move_sel(state, len, 1),
        KeyCode::PageUp => move_sel(state, len, -10),
        KeyCode::PageDown => move_sel(state, len, 10),
        KeyCode::Char('g') => {
            if len > 0 {
                state.select(Some(0));
            }
        }
        KeyCode::Char('G') => {
            if len > 0 {
                state.select(Some(len - 1));
            }
        }
        _ => {}
    }
}

fn list_len<T>(v: &Option<Vec<T>>) -> usize {
    v.as_ref().map(|x| x.len()).unwrap_or(0)
}

fn move_sel(state: &mut TableState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0) as i32;
    let next = (current + delta).clamp(0, len as i32 - 1);
    state.select(Some(next as usize));
}

fn selected_path(app: &App, indices: &[usize]) -> Option<PathBuf> {
    let sel = app.dash_state.selected()?;
    let repo_idx = *indices.get(sel)?;
    Some(PathBuf::from(&app.repos.get(repo_idx)?.path))
}

// ── Actions ───────────────────────────────────────────

fn run_single_sync<F>(app: &mut App, path: PathBuf, op: F, success_verb: &str)
where
    F: FnOnce(&std::path::Path) -> Result<(), crate::error::AppError>,
{
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    app.set_status(format!("working on {}…", name), Level::Info);

    match op(&path) {
        Ok(()) => {
            let info = git_status::get_repo_info(&path);
            if let Some(existing) = app.repos.iter_mut().find(|r| r.path == info.path) {
                *existing = info;
            }
            cache::save(&app.repos);
            app.set_status(format!("{} {}", success_verb, name), Level::Success);
        }
        Err(e) => app.set_status(format!("{}: {}", name, e), Level::Error),
    }
}

fn run_scan(app: &mut App) {
    let roots = cache::effective_scan_roots();
    if roots.is_empty() {
        app.set_status(
            "no scan roots configured (use: gitatlas config roots add)",
            Level::Error,
        );
        return;
    }
    app.set_status("scanning…", Level::Info);
    let repos = scanner::scan_roots(&roots);
    cache::save(&repos);
    let count = repos.len();
    app.repos = repos;
    app.set_status(format!("scanned {} repos", count), Level::Success);
}

fn start_bulk(app: &mut App, op: BulkOp) {
    if app.bulk.is_some() {
        app.set_status("bulk operation already running", Level::Warn);
        return;
    }
    if app.repos.is_empty() {
        app.set_status("no repos (run scan first)", Level::Warn);
        return;
    }

    let (tx, rx) = mpsc::channel();
    let repos: Vec<(String, String)> = app
        .repos
        .iter()
        .map(|r| (r.name.clone(), r.path.clone()))
        .collect();
    let total = repos.len();
    let op_verb = op.verb().to_string();

    let tx_done = tx.clone();
    thread::spawn(move || {
        repos.par_iter().for_each_with(tx.clone(), |tx, (name, path_str)| {
            let _ = tx.send(WorkerMsg::Progress { name: name.clone() });
            let path = PathBuf::from(path_str);
            let result = match op_verb.as_str() {
                "fetch" => operations::fetch_repo(&path),
                "pull" => operations::pull_rebase_repo(&path),
                _ => Ok(()),
            };
            let updated = git_status::get_repo_info(&path);
            let _ = tx.send(WorkerMsg::Done {
                path: path_str.clone(),
                name: name.clone(),
                error: result.err().map(|e| e.to_string()),
                updated,
            });
        });
        let _ = tx_done.send(WorkerMsg::Finished);
    });

    app.bulk = Some(BulkState {
        op,
        total,
        done: 0,
        current: None,
        errors: Vec::new(),
        rx,
    });
}

// ── Lazy tab loading ──────────────────────────────────

fn load_current_tab(app: &mut App) {
    let Some(detail) = app.detail.as_mut() else { return };
    let Some(repo) = app.repos.get(detail.repo_index) else { return };
    let path = PathBuf::from(&repo.path);

    match detail.tab {
        Tab::Changes => {
            if detail.changes.is_none() {
                match detail::get_file_changes(&path) {
                    Ok(c) => detail.changes = Some(c),
                    Err(e) => {
                        detail.changes = Some(Vec::new());
                        let msg = e.to_string();
                        app.set_status(msg, Level::Error);
                    }
                }
            }
        }
        Tab::History => {
            if detail.commits.is_none() {
                match detail::get_commit_log(&path, 200) {
                    Ok(c) => detail.commits = Some(c),
                    Err(e) => {
                        detail.commits = Some(Vec::new());
                        app.set_status(e.to_string(), Level::Error);
                    }
                }
            }
        }
        Tab::Branches => {
            if detail.branches.is_none() {
                match detail::get_branches(&path) {
                    Ok(b) => detail.branches = Some(b),
                    Err(e) => {
                        detail.branches = Some(Vec::new());
                        app.set_status(e.to_string(), Level::Error);
                    }
                }
            }
        }
        Tab::Stashes => {
            if detail.stashes.is_none() {
                match detail::get_stashes(&path) {
                    Ok(s) => detail.stashes = Some(s),
                    Err(e) => {
                        detail.stashes = Some(Vec::new());
                        app.set_status(e.to_string(), Level::Error);
                    }
                }
            }
        }
        Tab::Readme => {
            if detail.readme.is_none() {
                match detail::get_readme(&path) {
                    Ok(Some(text)) => detail.readme = Some(text),
                    Ok(None) => detail.readme = Some("(no README found)".to_string()),
                    Err(e) => {
                        detail.readme = Some(format!("(error: {})", e));
                    }
                }
            }
        }
    }
}

fn invalidate_current_tab(app: &mut App) {
    let Some(detail) = app.detail.as_mut() else { return };
    match detail.tab {
        Tab::Changes => detail.changes = None,
        Tab::History => detail.commits = None,
        Tab::Branches => detail.branches = None,
        Tab::Stashes => detail.stashes = None,
        Tab::Readme => {
            detail.readme = None;
            detail.readme_scroll = 0;
        }
    }
}
