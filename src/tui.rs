use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::{Frame, Terminal};
use rayon::prelude::*;

use crate::cache;
use crate::git::operations::ProgressEvent;
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
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
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
    const ALL: [Tab; 5] = [
        Tab::Changes,
        Tab::History,
        Tab::Branches,
        Tab::Stashes,
        Tab::Readme,
    ];
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

#[derive(Clone, Copy)]
enum SingleOp {
    Fetch,
    Pull,
    Push,
}

impl SingleOp {
    fn progress_verb(&self) -> &'static str {
        match self {
            SingleOp::Fetch => "fetching",
            SingleOp::Pull => "pulling",
            SingleOp::Push => "pushing",
        }
    }
    fn success_verb(&self) -> &'static str {
        match self {
            SingleOp::Fetch => "fetched",
            SingleOp::Pull => "pulled",
            SingleOp::Push => "pushed",
        }
    }
}

enum SingleEvent {
    Progress(ProgressEvent),
    Done(Result<RepoInfo, String>),
}

#[derive(Default)]
struct SingleProgress {
    stage: Option<String>,
    sideband: Option<String>,
    tip: Option<String>,
    transfer: Option<(usize, usize, usize, usize)>, // bytes, indexed, received, total objects
    push: Option<(usize, usize, usize)>,            // current, total, bytes
    rebase: Option<(usize, usize)>,                 // current, total
}

struct SingleState {
    op: SingleOp,
    name: String,
    path: String,
    started: Instant,
    progress: SingleProgress,
    rx: Receiver<SingleEvent>,
}

enum WorkerMsg {
    Progress {
        name: String,
    },
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
    single: Option<SingleState>,
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
            single: None,
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
        let Some(bulk) = self.bulk.as_mut() else {
            return;
        };
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

    fn drain_single(&mut self) {
        let Some(single) = self.single.as_mut() else {
            return;
        };
        loop {
            match single.rx.try_recv() {
                Ok(SingleEvent::Progress(ev)) => match ev {
                    ProgressEvent::Stage(s) => single.progress.stage = Some(s),
                    ProgressEvent::Transfer {
                        received_bytes,
                        indexed_objects,
                        received_objects,
                        total_objects,
                    } => {
                        single.progress.transfer = Some((
                            received_bytes,
                            indexed_objects,
                            received_objects,
                            total_objects,
                        ));
                    }
                    ProgressEvent::PushTransfer {
                        current,
                        total,
                        bytes,
                    } => {
                        single.progress.push = Some((current, total, bytes));
                    }
                    ProgressEvent::Sideband(s) => single.progress.sideband = Some(s),
                    ProgressEvent::Tip(s) => single.progress.tip = Some(s),
                    ProgressEvent::RebaseStep { current, total } => {
                        single.progress.rebase = Some((current, total));
                    }
                },
                Ok(SingleEvent::Done(result)) => {
                    let name = single.name.clone();
                    let path = single.path.clone();
                    let success_verb = single.op.success_verb();
                    let elapsed = single.started.elapsed();
                    self.single = None;
                    match result {
                        Ok(updated) => {
                            if let Some(existing) = self.repos.iter_mut().find(|r| r.path == path) {
                                *existing = updated;
                            }
                            cache::save(&self.repos);
                            self.set_status(
                                format!("{} {} in {}", success_verb, name, fmt_duration(elapsed)),
                                Level::Success,
                            );
                        }
                        Err(e) => self.set_status(format!("{}: {}", name, e), Level::Error),
                    }
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    let name = single.name.clone();
                    self.single = None;
                    self.set_status(format!("{}: worker disconnected", name), Level::Error);
                    return;
                }
            }
        }
    }
}

// ── Event loop ────────────────────────────────────────

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> anyhow::Result<()> {
    let mut app = App::new();
    loop {
        terminal.draw(|f| draw(f, &mut app))?;

        // Poll more often while an op is in flight so the spinner stays smooth
        // and progress updates render promptly.
        let poll_ms = if app.single.is_some() || app.bulk.is_some() {
            80
        } else {
            200
        };
        if event::poll(Duration::from_millis(poll_ms))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key);
                }
            }
        }

        app.drain_worker();
        app.drain_single();
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
        Some(h) => format!(
            "filter:{}",
            match h {
                RepoHealth::Clean => "clean",
                RepoHealth::Dirty => "dirty",
                RepoHealth::Diverged => "diverged",
                RepoHealth::Error => "error",
            }
        ),
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
    f.render_widget(Paragraph::new(right).alignment(Alignment::Right), cols[1]);
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

    if let Some(single) = &app.single {
        let label = format!(" {} ", single_progress_summary(single));
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
                name_cell(r),
                branch_cell(&r.branch),
                ahead_cell(r.ahead),
                behind_cell(r.behind),
                dirty_cell(r.dirty_files),
                stash_cell(r.stash_count),
                health_cell(r.health),
                path_cell(&r.path),
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
    .style(
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan),
    );

    let border_label = if let Some(h) = app.filter_health {
        format!(" repositories (filter: {}) ", health_name(h))
    } else {
        " repositories ".to_string()
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(border_label))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
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

    // Summary block — name tinted by health so dirty/diverged/error pop;
    // numeric counters colored individually (dim when zero).
    let name_color = if repo.health == RepoHealth::Clean {
        Color::Cyan
    } else {
        health_color(repo.health)
    };
    let counter_style = |n: u32, color: Color| {
        if n == 0 {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        }
    };
    let dim = Style::default().fg(Color::DarkGray);
    let summary_lines = vec![
        Line::from(vec![
            Span::styled(
                repo.name.clone(),
                Style::default().add_modifier(Modifier::BOLD).fg(name_color),
            ),
            Span::raw("  "),
            Span::styled("on ", dim),
            Span::styled(
                repo.branch.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("↑", dim),
            Span::styled(
                repo.ahead.to_string(),
                counter_style(repo.ahead, Color::Green),
            ),
            Span::raw(" "),
            Span::styled("↓", dim),
            Span::styled(
                repo.behind.to_string(),
                counter_style(repo.behind, Color::Red),
            ),
            Span::raw("   "),
            Span::styled("dirty ", dim),
            Span::styled(
                repo.dirty_files.to_string(),
                counter_style(repo.dirty_files, Color::Yellow),
            ),
            Span::raw("   "),
            Span::styled("stash ", dim),
            Span::styled(
                repo.stash_count.to_string(),
                counter_style(repo.stash_count, Color::Magenta),
            ),
            Span::raw("   "),
            Span::styled(
                health_name(repo.health),
                Style::default()
                    .fg(health_color(repo.health))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("path:   ", dim),
            Span::styled(repo.path.clone(), Style::default().fg(Color::DarkGray)),
        ]),
    ];
    f.render_widget(
        Paragraph::new(summary_lines).block(Block::default().borders(Borders::ALL)),
        chunks[0],
    );

    // Tabs
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
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
                Row::new(vec![
                    staged_cell(c.staged),
                    Cell::from(Span::styled(
                        c.status.short().to_string(),
                        file_status_style(c.status).add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(Span::styled(c.path.clone(), file_status_style(c.status))),
                ])
            })
            .collect(),
        Some(_) => {
            f.render_widget(
                Paragraph::new("(clean working tree)")
                    .block(Block::default().borders(Borders::ALL).title(" changes ")),
                area,
            );
            return;
        }
        None => {
            f.render_widget(
                Paragraph::new("loading…")
                    .block(Block::default().borders(Borders::ALL).title(" changes ")),
                area,
            );
            return;
        }
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(4),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(vec!["STAGED", "ST", "PATH"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
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
                Paragraph::new("loading…")
                    .block(Block::default().borders(Borders::ALL).title(" history ")),
                area,
            );
            return;
        }
    };

    if commits.is_empty() {
        f.render_widget(
            Paragraph::new("(no commits)")
                .block(Block::default().borders(Borders::ALL).title(" history ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = commits
        .iter()
        .map(|c| {
            let mut ref_spans: Vec<Span> = Vec::new();
            for (i, r) in c.refs.iter().enumerate() {
                if i > 0 {
                    ref_spans.push(Span::styled(",", Style::default().fg(Color::DarkGray)));
                }
                ref_spans.push(ref_span(r.kind, &r.name));
            }
            let msg = c.message.lines().next().unwrap_or("").to_string();
            let date = c.date.split('T').next().unwrap_or(&c.date).to_string();
            Row::new(vec![
                Cell::from(Span::styled(
                    c.short_oid.clone(),
                    Style::default().fg(Color::Yellow),
                )),
                Cell::from(Span::styled(date, Style::default().fg(Color::DarkGray))),
                Cell::from(Span::styled(
                    c.author.clone(),
                    Style::default().fg(Color::Cyan),
                )),
                Cell::from(Line::from(ref_spans)),
                Cell::from(msg),
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
    .header(
        Row::new(vec!["SHA", "DATE", "AUTHOR", "REFS", "MESSAGE"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
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
                Paragraph::new("loading…")
                    .block(Block::default().borders(Borders::ALL).title(" branches ")),
                area,
            );
            return;
        }
    };

    if branches.is_empty() {
        f.render_widget(
            Paragraph::new("(no branches)")
                .block(Block::default().borders(Borders::ALL).title(" branches ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = branches
        .iter()
        .map(|b| {
            let (kind, kind_color) = if b.is_head {
                ("HEAD", Color::Green)
            } else if b.is_remote {
                ("remote", Color::Yellow)
            } else {
                ("local", Color::Cyan)
            };
            let marker = if b.is_head {
                Cell::from(Span::styled(
                    "*".to_string(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Cell::from(" ".to_string())
            };
            let mut name_style = Style::default().fg(kind_color);
            if b.is_head {
                name_style = name_style.add_modifier(Modifier::BOLD);
            }
            Row::new(vec![
                marker,
                Cell::from(Span::styled(
                    kind.to_string(),
                    Style::default().fg(kind_color),
                )),
                Cell::from(Span::styled(b.name.clone(), name_style)),
                Cell::from(Span::styled(
                    b.upstream.clone().unwrap_or_default(),
                    Style::default().fg(Color::DarkGray),
                )),
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
    .header(
        Row::new(vec!["", "KIND", "NAME", "UPSTREAM"]).style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    )
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
                Paragraph::new("loading…")
                    .block(Block::default().borders(Borders::ALL).title(" stashes ")),
                area,
            );
            return;
        }
    };

    if stashes.is_empty() {
        f.render_widget(
            Paragraph::new("(no stashes)")
                .block(Block::default().borders(Borders::ALL).title(" stashes ")),
            area,
        );
        return;
    }

    let rows: Vec<Row> = stashes
        .iter()
        .map(|s| {
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("stash@{{{}}}", s.index),
                    Style::default().fg(Color::Magenta),
                )),
                Cell::from(s.message.clone()),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(12), Constraint::Min(20)])
        .header(
            Row::new(vec!["REF", "MESSAGE"]).style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        )
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
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            "Dashboard",
            Style::default().add_modifier(Modifier::BOLD),
        )),
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
        Line::from(Span::styled(
            "Detail",
            Style::default().add_modifier(Modifier::BOLD),
        )),
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

fn health_name(h: RepoHealth) -> &'static str {
    match h {
        RepoHealth::Clean => "clean",
        RepoHealth::Dirty => "dirty",
        RepoHealth::Diverged => "diverged",
        RepoHealth::Error => "error",
    }
}

// ── Theme helpers ─────────────────────────────────────

fn health_color(h: RepoHealth) -> Color {
    match h {
        RepoHealth::Clean => Color::Green,
        RepoHealth::Dirty => Color::Yellow,
        RepoHealth::Diverged => Color::Magenta,
        RepoHealth::Error => Color::Red,
    }
}

fn name_cell(repo: &RepoInfo) -> Cell<'static> {
    let mut style = Style::default().add_modifier(Modifier::BOLD);
    // Clean repos render plain — only repos needing attention get tinted.
    if repo.health != RepoHealth::Clean {
        style = style.fg(health_color(repo.health));
    }
    Cell::from(Span::styled(repo.name.clone(), style))
}

fn branch_cell(branch: &str) -> Cell<'static> {
    Cell::from(Span::styled(
        branch.to_string(),
        Style::default().fg(Color::Cyan),
    ))
}

fn count_cell(n: u32, on_nonzero: Style) -> Cell<'static> {
    let s = n.to_string();
    if n == 0 {
        Cell::from(Span::styled(s, Style::default().fg(Color::DarkGray)))
    } else {
        Cell::from(Span::styled(s, on_nonzero))
    }
}

fn ahead_cell(n: u32) -> Cell<'static> {
    count_cell(n, Style::default().fg(Color::Green))
}

fn behind_cell(n: u32) -> Cell<'static> {
    count_cell(n, Style::default().fg(Color::Red))
}

fn dirty_cell(n: u32) -> Cell<'static> {
    count_cell(
        n,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )
}

fn stash_cell(n: u32) -> Cell<'static> {
    count_cell(n, Style::default().fg(Color::Magenta))
}

fn health_cell(h: RepoHealth) -> Cell<'static> {
    Cell::from(Span::styled(
        health_name(h).to_string(),
        Style::default()
            .fg(health_color(h))
            .add_modifier(Modifier::BOLD),
    ))
}

fn path_cell(path: &str) -> Cell<'static> {
    Cell::from(Span::styled(
        path.to_string(),
        Style::default().fg(Color::DarkGray),
    ))
}

fn file_status_style(s: crate::models::FileStatus) -> Style {
    use crate::models::FileStatus::*;
    match s {
        Added => Style::default().fg(Color::Green),
        Modified => Style::default().fg(Color::Yellow),
        Deleted => Style::default().fg(Color::Red),
        Renamed => Style::default().fg(Color::Cyan),
        Untracked => Style::default().fg(Color::DarkGray),
        Conflicted => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn staged_cell(staged: bool) -> Cell<'static> {
    if staged {
        Cell::from(Span::styled(
            "staged".to_string(),
            Style::default().fg(Color::Green),
        ))
    } else {
        Cell::from(Span::styled(
            "unstaged".to_string(),
            Style::default().fg(Color::Yellow),
        ))
    }
}

fn ref_span(kind: RefKind, name: &str) -> Span<'static> {
    let (label, color, bold) = match kind {
        RefKind::Head => (format!("HEAD→{}", name), Color::Green, true),
        RefKind::Local => (name.to_string(), Color::Cyan, false),
        RefKind::Remote => (format!("r/{}", name), Color::Yellow, false),
        RefKind::Tag => (format!("tag:{}", name), Color::Magenta, false),
    };
    let mut style = Style::default().fg(color);
    if bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    Span::styled(label, style)
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
        KeyCode::Char('g') if !indices.is_empty() => {
            app.dash_state.select(Some(0));
        }
        KeyCode::Char('G') if !indices.is_empty() => {
            app.dash_state.select(Some(indices.len() - 1));
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
                start_single(app, path, SingleOp::Fetch);
            }
        }
        KeyCode::Char('p') => {
            if let Some(path) = selected_path(app, &indices) {
                start_single(app, path, SingleOp::Pull);
            }
        }
        KeyCode::Char('P') => {
            if let Some(path) = selected_path(app, &indices) {
                start_single(app, path, SingleOp::Push);
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
    let Some(detail) = app.detail.as_mut() else {
        return;
    };

    match detail.tab {
        Tab::Changes => nav_table(key.code, &mut detail.changes_sel, list_len(&detail.changes)),
        Tab::History => nav_table(key.code, &mut detail.commits_sel, list_len(&detail.commits)),
        Tab::Branches => nav_table(
            key.code,
            &mut detail.branches_sel,
            list_len(&detail.branches),
        ),
        Tab::Stashes => nav_table(key.code, &mut detail.stashes_sel, list_len(&detail.stashes)),
        Tab::Readme => match key.code {
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
        },
    }

    // Detail-level remote ops on current repo
    if let Some(repo) = app
        .repos
        .get(app.detail.as_ref().unwrap().repo_index)
        .cloned()
    {
        let path = PathBuf::from(&repo.path);
        match key.code {
            KeyCode::Char('f') => start_single(app, path, SingleOp::Fetch),
            KeyCode::Char('p') => start_single(app, path, SingleOp::Pull),
            KeyCode::Char('P') => start_single(app, path, SingleOp::Push),
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
        KeyCode::Char('g') if len > 0 => {
            state.select(Some(0));
        }
        KeyCode::Char('G') if len > 0 => {
            state.select(Some(len - 1));
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

fn start_single(app: &mut App, path: PathBuf, op: SingleOp) {
    if app.single.is_some() {
        app.set_status("another operation is already running", Level::Warn);
        return;
    }
    if app.bulk.is_some() {
        app.set_status("bulk operation in progress", Level::Warn);
        return;
    }

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let path_str = path.display().to_string();

    let (event_tx, rx) = mpsc::channel::<SingleEvent>();

    // Bridge ProgressEvents from operations into our SingleEvent::Progress wrapper.
    let (prog_tx, prog_rx) = mpsc::channel::<ProgressEvent>();
    let bridge_tx = event_tx.clone();
    thread::spawn(move || {
        while let Ok(ev) = prog_rx.recv() {
            if bridge_tx.send(SingleEvent::Progress(ev)).is_err() {
                break;
            }
        }
    });

    let worker_path = path.clone();
    let done_tx = event_tx;
    thread::spawn(move || {
        let result = match op {
            SingleOp::Fetch => operations::fetch_repo_with_progress(&worker_path, Some(prog_tx)),
            SingleOp::Pull => {
                operations::pull_rebase_repo_with_progress(&worker_path, Some(prog_tx))
            }
            SingleOp::Push => operations::push_repo_with_progress(&worker_path, Some(prog_tx)),
        };
        let payload = result
            .map(|()| git_status::get_repo_info(&worker_path))
            .map_err(|e| e.to_string());
        let _ = done_tx.send(SingleEvent::Done(payload));
    });

    app.single = Some(SingleState {
        op,
        name,
        path: path_str,
        started: Instant::now(),
        progress: SingleProgress::default(),
        rx,
    });
}

const SPINNER: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

fn spinner_frame(elapsed: Duration) -> char {
    let idx = (elapsed.as_millis() / 80) as usize % SPINNER.len();
    SPINNER[idx]
}

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else if secs > 0 {
        format!("{}.{}s", secs, d.subsec_millis() / 100)
    } else {
        format!("{}ms", d.as_millis())
    }
}

fn fmt_bytes(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * KB;
    const GB: usize = 1024 * MB;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{} B", n)
    }
}

fn single_progress_summary(single: &SingleState) -> String {
    let elapsed = single.started.elapsed();
    let mut parts: Vec<String> = Vec::new();

    if let Some(stage) = &single.progress.stage {
        parts.push(stage.clone());
    } else {
        parts.push(single.op.progress_verb().to_string());
    }

    if let Some((bytes, indexed, received, total)) = single.progress.transfer {
        if total > 0 {
            parts.push(format!(
                "{} • {}/{} objects (indexed {})",
                fmt_bytes(bytes),
                received,
                total,
                indexed,
            ));
        } else if bytes > 0 {
            parts.push(fmt_bytes(bytes));
        }
    }

    if let Some((current, total, bytes)) = single.progress.push {
        if total > 0 {
            parts.push(format!(
                "{}/{} objects • {}",
                current,
                total,
                fmt_bytes(bytes)
            ));
        }
    }

    if let Some((current, total)) = single.progress.rebase {
        if total > 0 {
            parts.push(format!("rebase {}/{}", current, total));
        }
    }

    if let Some(tip) = &single.progress.tip {
        parts.push(tip.clone());
    } else if let Some(sb) = &single.progress.sideband {
        parts.push(sb.clone());
    }

    parts.push(fmt_duration(elapsed));

    format!(
        "{} {} {}",
        spinner_frame(elapsed),
        single.name,
        parts.join(" • "),
    )
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
    if app.single.is_some() {
        app.set_status("another operation is already running", Level::Warn);
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
        repos
            .par_iter()
            .for_each_with(tx.clone(), |tx, (name, path_str)| {
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
    let Some(detail) = app.detail.as_mut() else {
        return;
    };
    let Some(repo) = app.repos.get(detail.repo_index) else {
        return;
    };
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
    let Some(detail) = app.detail.as_mut() else {
        return;
    };
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
