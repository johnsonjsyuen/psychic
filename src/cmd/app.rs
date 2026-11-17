//! The picker's UI thread: state, start-up, the event loop and key handling
//! (`docs/ctrl-r/02-browse.md` sections 4, 8 and 9).
//!
//! The worker is spawned before the terminal is touched, so the index loads
//! while the terminal is being set up. The list on screen changes only when
//! the worker answers a query the user caused: there is no timer-driven
//! refresh (ADR-11). The pick is printed to stdout after the terminal is
//! restored; stdout is the shell widget's pipe.

use super::events::{CMD_EVENT_WAIT_MS, CmdEvents, PickPlace, Shown};
use super::index::LoadParams;
use super::keys::{self, CmdAction};
use super::render;
use super::store::CmdStore;
use super::terminal::{self, PickerTerminal, TerminationSignals, Tui};
use super::train::{self, CmdModelStats};
use super::tuning::{CMD_PAGE_PREFETCH, CMD_TICK_MS};
use super::worker::{self, CmdDetails, CmdRequest, CmdResponse, CmdRow, Scope};
use super::{cli, privacy, repo};
use anyhow::Result;
use crossterm::event::{Event, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Everything the event loop waits for.
pub(super) enum CmdEvent {
    Input(Event),
    Worker(CmdResponse),
    Tick,
    /// SIGTERM, SIGHUP or SIGINT arrived (E-B-21).
    Terminate(std::os::raw::c_int),
}

/// How the event loop ended.
enum Ended {
    /// Enter (with the command) or Esc (without).
    Pick(Option<Arc<str>>),
    /// A termination signal: give the terminal back, print nothing, and die
    /// of the signal.
    Signal(std::os::raw::c_int),
}

/// Set in the environment, the picker panics in its first draw: how
/// `tests/picker.rs` checks that a panic gives the terminal back (E-B-20).
const TEST_PANIC_ENV: &str = "PSYCHIC_HISTORY_TEST_PANIC";

impl From<Event> for CmdEvent {
    fn from(event: Event) -> Self {
        CmdEvent::Input(event)
    }
}

impl From<CmdResponse> for CmdEvent {
    fn from(response: CmdResponse) -> Self {
        CmdEvent::Worker(response)
    }
}

/// From `CmdResponse::Loaded`.
pub(super) struct LoadInfo {
    pub commands: usize,
    pub load_ms: f64,
}

/// What the debug pane reports about speed.
#[derive(Default)]
pub(super) struct CmdTimings {
    pub first_results_ms: Option<f64>,
    pub last_rank_ms: Option<f64>,
    pub last_round_trip_ms: Option<f64>,
}

/// What a key or event asks the loop to do.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Flow {
    Continue,
    /// Leave, printing the command if there is one.
    Exit(Option<Arc<str>>),
}

pub(super) struct CmdApp {
    pub query: String,
    pub scope: Scope,
    /// Contiguous from rank 0.
    pub rows: Vec<CmdRow>,
    pub total_matched: usize,
    pub total_commands: usize,
    pub selected: usize,
    pub scroll: usize,
    pub visible_rows: usize,
    /// Id of the `QueryUpdated` the rows came from; 0 = none yet.
    pub shown_query_id: u64,
    /// Starts at 2 (1 is the initial query).
    pub next_query_id: u64,
    /// Highest `start` already asked for with `GetRows`.
    pub rows_requested_to: usize,
    /// Command awaiting the one-key confirmation of a delete.
    pub confirm_delete: Option<i64>,
    /// A `Delete` was sent this session: at exit the write-ahead log is
    /// emptied once more, in case the worker's own try found it busy.
    pub deleted_any: bool,
    pub help_visible: bool,
    pub debug_visible: bool,
    pub details: FxHashMap<i64, CmdDetails>,
    pub details_requested: FxHashSet<i64>,
    /// "now" for time-ago; steps once a minute on Tick.
    pub list_clock: i64,
    pub status: Option<String>,
    pub loaded: Option<LoadInfo>,
    /// The model the worker ranks with, from `Loaded`; `None` = heuristic only.
    pub model: Option<CmdModelStats>,
    /// The worker could not read the history (E-B-01); the list says so.
    pub load_failed: bool,
    /// The privacy marker / `PSYCHIC_HISTORY`, read once at start.
    pub recording_on: bool,
    pub timings: CmdTimings,
    pub home: Option<String>,
    pub cwd: String,
    pub events: CmdEvents,
    worker_tx: Sender<CmdRequest>,
    query_sent_at: Option<Instant>,
    /// The worker is gone (E-B-04); the list stays as it is.
    worker_stopped: bool,
}

/// Where `--cwd`, `$PWD` and `current_dir` put the picker (ADR-7: never
/// canonicalised). Empty when none of them says (E-B-11).
fn resolve_cwd(flag: Option<String>, pwd: Option<String>) -> String {
    if let Some(cwd) = flag {
        return cwd;
    }
    if let Some(pwd) = pwd.filter(|pwd| pwd.starts_with('/')) {
        return pwd;
    }
    match std::env::current_dir() {
        Ok(dir) => dir.to_string_lossy().into_owned(),
        Err(_) => {
            log::warn!(
                "the picker's directory is unknown: no --cwd, no $PWD, no current directory"
            );
            String::new()
        }
    }
}

impl CmdApp {
    pub(super) fn new(
        worker_tx: Sender<CmdRequest>,
        events: CmdEvents,
        query: String,
        now: i64,
        recording_on: bool,
        home: Option<String>,
        cwd: String,
    ) -> Self {
        CmdApp {
            query,
            scope: Scope::All,
            rows: Vec::new(),
            total_matched: 0,
            total_commands: 0,
            selected: 0,
            scroll: 0,
            visible_rows: 0,
            shown_query_id: 0,
            next_query_id: 2,
            rows_requested_to: 0,
            confirm_delete: None,
            deleted_any: false,
            help_visible: false,
            debug_visible: false,
            details: FxHashMap::default(),
            details_requested: FxHashSet::default(),
            list_clock: now,
            status: None,
            loaded: None,
            model: None,
            load_failed: false,
            recording_on,
            timings: CmdTimings::default(),
            home,
            cwd,
            events,
            worker_tx,
            query_sent_at: Some(Instant::now()),
            worker_stopped: false,
        }
    }

    /// Whether the worker has answered a query yet.
    pub fn has_results(&self) -> bool {
        self.shown_query_id > 0
    }

    pub fn selected_row(&self) -> Option<&CmdRow> {
        self.rows.get(self.selected)
    }

    /// The rows on screen, with their 1-based ranks.
    pub fn shown(&self) -> Vec<Shown> {
        let end = (self.scroll + self.visible_rows).min(self.rows.len());
        (self.scroll..end)
            .map(|i| Shown {
                command_id: self.rows[i].command_id,
                rank: i + 1,
            })
            .collect()
    }

    fn send(&mut self, request: CmdRequest) {
        if self.worker_tx.send(request).is_err() {
            self.worker_died();
        }
    }

    /// E-B-04: the list stays as it is and the keys keep working.
    fn worker_died(&mut self) {
        if !self.worker_stopped {
            log::error!("the history worker stopped");
            self.worker_stopped = true;
            self.status = Some("History worker stopped".to_string());
        }
    }

    /// Ask for the current query and scope under a new id.
    fn request_query(&mut self) {
        let query_id = self.next_query_id;
        self.next_query_id += 1;
        self.query_sent_at = Some(Instant::now());
        self.send(CmdRequest::UpdateQuery {
            query: self.query.clone(),
            query_id,
            scope: self.scope,
        });
    }

    fn adjust_scroll(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.visible_rows > 0 && self.selected >= self.scroll + self.visible_rows {
            self.scroll = self.selected + 1 - self.visible_rows;
        }
    }

    /// The window changed size: keep the selection on screen.
    pub fn set_visible_rows(&mut self, visible_rows: usize) {
        self.visible_rows = visible_rows;
        self.adjust_scroll();
    }

    /// After each draw: details for the selected row, and the next page when
    /// the selection nears the end of what is loaded.
    pub fn request_after_draw(&mut self) {
        if let Some(command_id) = self.selected_row().map(|row| row.command_id)
            && !self.details.contains_key(&command_id)
            && self.details_requested.insert(command_id)
        {
            self.send(CmdRequest::Details { command_id });
        }
        let loaded = self.rows.len();
        if self.selected + CMD_PAGE_PREFETCH >= loaded
            && loaded < self.total_matched
            && loaded > self.rows_requested_to
        {
            self.rows_requested_to = loaded;
            self.send(CmdRequest::GetRows {
                query_id: self.shown_query_id,
                start: loaded,
            });
        }
    }

    /// Apply a worker response. Returns whether it was a `QueryUpdated` that
    /// was accepted.
    pub fn on_worker(&mut self, response: CmdResponse) -> bool {
        match response {
            CmdResponse::QueryUpdated {
                query_id,
                query,
                total_matched,
                total_commands,
                rows,
                rank_ms,
            } => {
                if query_id < self.shown_query_id {
                    return false;
                }
                self.rows = rows;
                self.total_matched = total_matched;
                self.total_commands = total_commands;
                self.shown_query_id = query_id;
                self.selected = 0;
                self.scroll = 0;
                self.rows_requested_to = 0;
                // The confirmation names a row by what was selected; the
                // selection just moved, so it would delete another one.
                self.confirm_delete = None;
                self.events.new_subsession(query_id, query);
                self.timings.last_rank_ms = Some(rank_ms);
                self.timings.last_round_trip_ms = self
                    .query_sent_at
                    .map(|sent| sent.elapsed().as_secs_f64() * 1000.0);
                return true;
            }
            CmdResponse::Rows {
                query_id,
                start,
                rows,
            } => {
                if query_id == self.shown_query_id && start == self.rows.len() {
                    self.rows.extend(rows);
                }
            }
            CmdResponse::Details(details) => {
                // The worker answers only the newest of the Details queued
                // together; forget the others so a row selected again asks again.
                self.details_requested.clear();
                self.details.insert(details.command_id, details);
            }
            CmdResponse::Loaded {
                commands,
                load_ms,
                model,
            } => {
                self.loaded = Some(LoadInfo { commands, load_ms });
                self.model = model;
            }
            CmdResponse::LoadFailed { message } => {
                self.load_failed = true;
                self.status = Some(format!("Could not read history: {}", message));
            }
            CmdResponse::DeleteFailed => {
                self.status = Some("Could not delete".to_string());
            }
            CmdResponse::ModelDropped => self.model = None,
        }
        false
    }

    /// Step `list_clock` to the current minute, never backwards.
    pub fn on_tick(&mut self, now: i64) {
        let minute = now - now.rem_euclid(60);
        self.list_clock = self.list_clock.max(minute);
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.confirm_delete.is_some() || self.help_visible {
            return; // the selection must not move under a question or the help
        }
        match keys::lookup_mouse(mouse.kind) {
            Some(CmdAction::MoveUp) => self.move_selection(-1),
            Some(CmdAction::MoveDown) => self.move_selection(1),
            _ => {}
        }
    }

    fn move_selection(&mut self, by: isize) {
        let last = self.rows.len().min(self.total_matched).saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(by).min(last);
        self.adjust_scroll();
    }

    /// Handle one key press (section 9).
    pub fn on_key(&mut self, key: KeyEvent) -> Flow {
        if key.kind != KeyEventKind::Press {
            return Flow::Continue;
        }
        if let Some(command_id) = self.confirm_delete.take() {
            let plain = !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            if plain && matches!(key.code, crossterm::event::KeyCode::Char('y' | 'Y')) {
                let query_id = self.next_query_id;
                self.next_query_id += 1;
                self.query_sent_at = Some(Instant::now());
                self.send(CmdRequest::Delete {
                    command_id,
                    query_id,
                });
                self.deleted_any = true;
            }
            return Flow::Continue;
        }
        if self.help_visible {
            self.help_visible = false;
            return Flow::Continue;
        }
        self.status = None;

        let Some(action) = keys::lookup(key.code, key.modifiers) else {
            return Flow::Continue;
        };
        match action {
            CmdAction::AppendToQuery => {
                if let crossterm::event::KeyCode::Char(c) = key.code {
                    self.query.push(c);
                    self.request_query();
                }
            }
            CmdAction::DeleteFromQuery => {
                if self.query.pop().is_some() {
                    self.request_query();
                }
            }
            CmdAction::ClearQuery => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.request_query();
                }
            }
            CmdAction::MoveUp => self.move_selection(-1),
            CmdAction::MoveDown => self.move_selection(1),
            CmdAction::Accept => return self.accept(),
            CmdAction::ScopeForward => {
                self.scope = self.scope.next();
                self.request_query();
            }
            CmdAction::ScopeBackward => {
                self.scope = self.scope.prev();
                self.request_query();
            }
            CmdAction::DeleteSelected => {
                self.confirm_delete = self.selected_row().map(|row| row.command_id);
            }
            CmdAction::ToggleDebug => self.debug_visible = !self.debug_visible,
            CmdAction::ToggleHelp => self.help_visible = !self.help_visible,
            CmdAction::Cancel => return Flow::Exit(None),
        }
        Flow::Continue
    }

    /// Enter: log the impressions (forced) and the pick, then leave with the
    /// text. Logging failures never stop the pick (E-B-06).
    fn accept(&mut self) -> Flow {
        let Some(row) = self.selected_row() else {
            return Flow::Exit(None);
        };
        let picked = Shown {
            command_id: row.command_id,
            rank: self.selected + 1,
        };
        let text = Arc::clone(&row.text);
        let shown = if self.visible_rows > 0 && self.events.wants_impressions(true) {
            self.shown()
        } else {
            Vec::new()
        };
        // One transaction, waiting at most CMD_EVENT_WAIT_MS for the lock.
        if let Err(error) = self.events.log_accept(&shown, picked) {
            log::error!("command event logging failed: {}", error);
        }
        Flow::Exit(Some(text))
    }

    /// The top-of-loop impression check (section 8.2).
    fn log_impressions_if_due(&mut self) {
        if self.visible_rows > 0 && self.events.wants_impressions(false) {
            let shown = self.shown();
            if let Err(error) = self.events.log_impressions(&shown) {
                log::error!("command event logging failed: {}", error);
            }
        }
    }
}

fn since_start_ms() -> f64 {
    crate::PROCESS_START.elapsed().as_secs_f64() * 1000.0
}

/// `TIMING {"op":"<name>","ms":<since process start>}`, once per process.
fn log_once(logged: &mut bool, op: &str) {
    if !*logged {
        *logged = true;
        log::info!(
            "TIMING {{\"op\":\"{}\",\"ms\":{:.3}}}",
            op,
            since_start_ms()
        );
    }
}

/// Stop and join the worker. `false` if it had panicked.
fn stop_worker(worker_tx: &Sender<CmdRequest>, handle: JoinHandle<()>) -> bool {
    let _ = worker_tx.send(CmdRequest::Shutdown);
    match handle.join() {
        Ok(()) => true,
        Err(_) => {
            log::error!("the history worker panicked");
            false
        }
    }
}

/// `psychic history` with no action flag: the picker.
pub(super) fn run(data_dir: &Path, args: &cli::HistoryArgs) -> Result<()> {
    // 1. Inputs, resolved once.
    let now = jiff::Timestamp::now().as_second();
    let home = std::env::var("HOME").ok();
    let cwd = resolve_cwd(args.cwd.clone(), std::env::var("PWD").ok());
    let repo_root = (!cwd.is_empty())
        .then(|| repo::find_repo_root(Path::new(&cwd), home.as_deref().map(Path::new)))
        .flatten()
        .map(|root: PathBuf| root.to_string_lossy().into_owned());
    let recording_on =
        privacy::recording_enabled(data_dir, std::env::var("PSYCHIC_HISTORY").ok().as_deref());
    let tui_session_id = rand::random::<u64>().to_string();
    let (event_tx, event_rx) = mpsc::channel::<CmdEvent>();

    // 2-3. The worker loads while the terminal is set up.
    let params = LoadParams {
        now,
        cwd: (!cwd.is_empty()).then(|| cwd.clone()),
        repo_root: repo_root.clone(),
        shell_session: args.session.clone(),
    };
    let (worker_tx, worker_handle) = worker::spawn(data_dir, params, event_tx.clone());
    let _ = worker_tx.send(CmdRequest::UpdateQuery {
        query: args.query.clone(),
        query_id: 1,
        scope: Scope::All,
    });

    // 4-5. Input thread and terminal.
    crate::tty_input::attach_stdin_to_terminal();
    let mut input = match crate::tty_input::spawn(event_tx.clone()) {
        Ok(input) => input,
        Err(error) => {
            stop_worker(&worker_tx, worker_handle);
            return Err(error);
        }
    };
    // From here on, every way out gives the terminal back (E-B-20, E-B-21).
    terminal::restore_on_panic();
    let signal_tx = event_tx.clone();
    let signals = TerminationSignals::watch(move |signal| {
        let _ = signal_tx.send(CmdEvent::Terminate(signal));
    });
    let set_up = signals.and_then(|signals| Ok((signals, PickerTerminal::enter()?)));
    let (_signals, mut terminal) = match set_up {
        Ok(set_up) => set_up,
        Err(error) => {
            stop_worker(&worker_tx, worker_handle);
            input.shutdown();
            return Err(error);
        }
    };

    // 6. The UI thread's own connection, for event logging, and the tick.
    let store = match CmdStore::open(data_dir).and_then(|store| {
        store.set_busy_timeout(Duration::from_millis(CMD_EVENT_WAIT_MS))?;
        Ok(store)
    }) {
        Ok(store) => Some(store),
        Err(error) => {
            log::error!("command events will not be logged: {}", error);
            None
        }
    };
    let place = PickPlace {
        cwd: cwd.clone(),
        repo_root,
        shell_session: args.session.clone(),
    };
    let events = CmdEvents::new(tui_session_id, place, store);
    let tick_tx = event_tx.clone();
    std::thread::spawn(move || {
        while tick_tx.send(CmdEvent::Tick).is_ok() {
            std::thread::sleep(Duration::from_millis(CMD_TICK_MS));
        }
    });
    drop(event_tx);

    let mut app = CmdApp::new(
        worker_tx.clone(),
        events,
        args.query.clone(),
        now,
        recording_on,
        home,
        cwd,
    );

    // 7. The event loop.
    if std::env::var_os(TEST_PANIC_ENV).is_some() {
        terminal.draw(|_| panic!("{TEST_PANIC_ENV} is set"))?;
    }
    let result = event_loop(&mut terminal, &mut app, &event_rx, &worker_handle, data_dir);

    // 8. Give the terminal back and print first: the worker may be busy
    // (a load, a delete waiting on a lock) and the shell waits for nothing
    // it does. Then ask it to stop; it is joined only if already done, as
    // the process is about to exit anyway.
    input.shutdown();
    terminal.leave()?;
    let printed = match result {
        Ok(Ended::Signal(signal)) => {
            log::info!("the picker got signal {}; nothing is printed", signal);
            terminal::die_of(signal);
        }
        Ok(Ended::Pick(Some(text))) => {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            writeln!(out, "{}", text)
                .and_then(|()| out.flush())
                .map_err(anyhow::Error::from)
        }
        Ok(Ended::Pick(None)) => Ok(()),
        Err(error) => Err(error),
    };
    let _ = worker_tx.send(CmdRequest::Shutdown);
    if worker_handle.is_finished() && worker_handle.join().is_err() {
        log::error!("the history worker panicked");
    }
    if app.deleted_any {
        app.events.truncate_log();
    }
    drop(app);
    printed
}

fn event_loop(
    terminal: &mut Tui,
    app: &mut CmdApp,
    event_rx: &Receiver<CmdEvent>,
    worker: &JoinHandle<()>,
    data_dir: &Path,
) -> Result<Ended> {
    let (mut first_render, mut first_results, mut first_full_render) = (false, false, false);
    // When the query whose results arrived was sent: the next draw shows them.
    let mut results_to_draw: Option<Instant> = None;
    loop {
        app.log_impressions_if_due();
        let mut layout = render::CmdLayout { visible_rows: 0 };
        let draw_started = Instant::now();
        terminal.draw(|f| layout = render::draw(f, app))?;
        app.set_visible_rows(layout.visible_rows);
        if let Some(sent) = results_to_draw.take() {
            // Query sent (on the key) to its results drawn: what the user
            // waits for after a keystroke (bench/history.py, 04 section 6.3),
            // and how much of it the draw itself took.
            log::info!(
                "TIMING {{\"op\":\"cmd_key_to_results\",\"ms\":{:.3},\"draw_ms\":{:.3}}}",
                sent.elapsed().as_secs_f64() * 1000.0,
                draw_started.elapsed().as_secs_f64() * 1000.0
            );
        }
        log_once(&mut first_render, "cmd_first_render");
        if app.has_results() {
            log_once(&mut first_full_render, "cmd_first_full_render");
        }
        if worker.is_finished() {
            app.worker_died();
        }
        app.request_after_draw();

        let Ok(event) = event_rx.recv() else {
            return Ok(Ended::Pick(None)); // every sender is gone: nothing more can happen
        };
        match event {
            CmdEvent::Worker(response) => {
                let sent = app.query_sent_at;
                if app.on_worker(response) {
                    results_to_draw = sent;
                    if !first_results {
                        app.timings.first_results_ms = Some(since_start_ms());
                        // Training never runs in the picker (ADR-10): this
                        // thread at most starts a detached child.
                        let data_dir = data_dir.to_path_buf();
                        std::thread::spawn(move || {
                            train::maybe_spawn_retrain(&data_dir, std::time::SystemTime::now());
                        });
                    }
                    log_once(&mut first_results, "cmd_first_results");
                }
            }
            CmdEvent::Tick => app.on_tick(jiff::Timestamp::now().as_second()),
            CmdEvent::Terminate(signal) => return Ok(Ended::Signal(signal)),
            CmdEvent::Input(Event::Key(key)) => {
                if let Flow::Exit(text) = app.on_key(key) {
                    return Ok(Ended::Pick(text));
                }
            }
            CmdEvent::Input(Event::Mouse(mouse)) => app.on_mouse(mouse),
            CmdEvent::Input(_) => {} // a resize: the next draw picks it up
        }
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use crate::cmd::rank::HeuristicParts;

    /// A row with sensible defaults.
    pub(in crate::cmd) fn row(command_id: i64, text: &str) -> CmdRow {
        CmdRow {
            command_id,
            text: Arc::from(text),
            last_run_at: 999_000,
            run_count: 3,
            pick_count: 0,
            runs_here: 0,
            last_exit_code: Some(0),
            score: 0.5,
            heuristic: 0.5,
            heuristic_parts: HeuristicParts::default(),
            ml_score: None,
            ml_weight: 0.0,
            fuzzy_score: None,
            features: Vec::new(),
        }
    }

    /// An app with no store and a channel standing in for the worker.
    pub(in crate::cmd) fn app() -> (CmdApp, Receiver<CmdRequest>) {
        let (tx, rx) = mpsc::channel();
        let place = PickPlace {
            cwd: "/w".to_string(),
            repo_root: None,
            shell_session: None,
        };
        let events = CmdEvents::new("tui".to_string(), place, None);
        let app = CmdApp::new(
            tx,
            events,
            String::new(),
            1_000_000,
            true,
            Some("/home/u".to_string()),
            "/w".to_string(),
        );
        (app, rx)
    }

    pub(in crate::cmd) fn updated(query_id: u64, rows: Vec<CmdRow>) -> CmdResponse {
        let total = rows.len();
        CmdResponse::QueryUpdated {
            query_id,
            query: String::new(),
            total_matched: total,
            total_commands: total,
            rows,
            rank_ms: 0.5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crossterm::event::KeyCode;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn plain(c: char) -> KeyEvent {
        press(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn requests(rx: &Receiver<CmdRequest>) -> Vec<String> {
        rx.try_iter()
            .map(|request| match request {
                CmdRequest::UpdateQuery {
                    query,
                    query_id,
                    scope,
                } => format!("UpdateQuery({query:?}, {query_id}, {scope:?})"),
                CmdRequest::GetRows { query_id, start } => format!("GetRows({query_id}, {start})"),
                CmdRequest::Details { command_id } => format!("Details({command_id})"),
                CmdRequest::Delete {
                    command_id,
                    query_id,
                } => format!("Delete({command_id}, {query_id})"),
                CmdRequest::Shutdown => "Shutdown".to_string(),
            })
            .collect()
    }

    fn rows(n: usize) -> Vec<CmdRow> {
        (0..n)
            .map(|i| row(i as i64 + 1, &format!("c{i}")))
            .collect()
    }

    #[test]
    fn a_dropped_model_leaves_the_debug_pane() {
        let (mut app, _rx) = app();
        let stats = CmdModelStats {
            trained_at: "t".to_string(),
            trained_at_unix: 0,
            training_duration_seconds: 0.0,
            feature_names: Vec::new(),
            num_rows: 0,
            pick_groups: 0,
            hook_groups: 300,
            import_groups: 0,
            blend_examples: 300.0,
            best_iteration: 1,
            validation_ndcg: Vec::new(),
            feature_gain: Vec::new(),
            seed: 42,
        };
        app.on_worker(CmdResponse::Loaded {
            commands: 1,
            load_ms: 1.0,
            model: Some(stats),
        });
        assert!(app.model.is_some());
        assert!(!app.on_worker(CmdResponse::ModelDropped));
        assert!(app.model.is_none());
    }

    #[test]
    fn tc_b_10_stale_responses_are_dropped() {
        let (mut app, _rx) = app();
        app.on_worker(updated(5, rows(3)));
        app.selected = 2;
        assert!(!app.on_worker(updated(4, rows(1))));
        assert_eq!((app.rows.len(), app.selected), (3, 2));
        assert!(app.on_worker(updated(6, rows(2))));
        assert_eq!(
            (app.rows.len(), app.selected, app.shown_query_id),
            (2, 0, 6)
        );
        app.on_worker(CmdResponse::Rows {
            query_id: 5,
            start: 2,
            rows: rows(1),
        });
        assert_eq!(app.rows.len(), 2, "rows of an older query are ignored");
        app.on_worker(CmdResponse::Rows {
            query_id: 6,
            start: 7,
            rows: rows(1),
        });
        assert_eq!(
            app.rows.len(),
            2,
            "rows that do not continue the list are ignored"
        );
        app.selected = 1;
        app.on_worker(CmdResponse::Rows {
            query_id: 6,
            start: 2,
            rows: rows(1),
        });
        assert_eq!(
            (app.rows.len(), app.selected),
            (3, 1),
            "a page never moves the cursor"
        );
    }

    #[test]
    fn tc_b_12_delete_asks_first_and_help_swallows_one_key() {
        let (mut app, rx) = app();
        app.on_worker(updated(1, rows(3)));
        app.confirm_delete = Some(7);
        assert_eq!(app.on_key(plain('n')), Flow::Continue);
        assert_eq!(app.confirm_delete, None);
        assert!(requests(&rx).is_empty());
        assert_eq!(app.query, "", "the key was consumed");
        assert!(!app.deleted_any);

        app.confirm_delete = Some(7);
        app.on_key(plain('y'));
        assert_eq!(requests(&rx), ["Delete(7, 2)"]);
        assert_eq!(app.next_query_id, 3);
        assert!(app.deleted_any, "the log is emptied again at exit");

        // A new answer moves the selection, so it cancels the question; so
        // does nothing a wheel does, which cannot move it meanwhile.
        app.on_key(press(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(press(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.confirm_delete, Some(2));
        let wheel = MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(wheel);
        assert_eq!((app.selected, app.confirm_delete), (1, Some(2)));
        app.on_worker(updated(5, rows(3)));
        assert_eq!((app.selected, app.confirm_delete), (0, None));

        app.help_visible = true;
        assert_eq!(
            app.on_key(press(KeyCode::Esc, KeyModifiers::NONE)),
            Flow::Continue
        );
        assert!(
            !app.help_visible,
            "Esc closed help and the picker is still running"
        );
    }

    #[test]
    fn delete_selected_targets_the_selected_row_and_does_nothing_on_an_empty_list() {
        let (mut app, _rx) = app();
        app.on_key(press(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(app.confirm_delete, None);
        app.on_worker(updated(1, rows(3)));
        app.on_key(press(KeyCode::Down, KeyModifiers::NONE));
        app.on_key(press(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(app.confirm_delete, Some(2));
    }

    #[test]
    fn typing_and_scopes_send_new_queries_with_rising_ids() {
        let (mut app, rx) = app();
        app.on_key(plain('g'));
        app.on_key(press(KeyCode::Char('S'), KeyModifiers::SHIFT));
        app.on_key(press(KeyCode::Backspace, KeyModifiers::NONE));
        app.on_key(press(KeyCode::Tab, KeyModifiers::NONE));
        app.on_key(press(KeyCode::BackTab, KeyModifiers::SHIFT));
        app.on_key(press(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.on_key(press(KeyCode::Char('u'), KeyModifiers::CONTROL));
        app.on_key(press(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(
            requests(&rx),
            [
                "UpdateQuery(\"g\", 2, All)",
                "UpdateQuery(\"gS\", 3, All)",
                "UpdateQuery(\"g\", 4, All)",
                "UpdateQuery(\"g\", 5, Directory)",
                "UpdateQuery(\"g\", 6, All)",
                "UpdateQuery(\"\", 7, All)",
            ]
        );
    }

    #[test]
    fn moving_stops_at_both_ends_and_scrolls() {
        let (mut app, _rx) = app();
        app.on_worker(updated(1, rows(5)));
        app.set_visible_rows(2);
        app.on_key(press(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!((app.selected, app.scroll), (0, 0));
        for _ in 0..10 {
            app.on_key(press(KeyCode::Char('n'), KeyModifiers::CONTROL));
        }
        assert_eq!((app.selected, app.scroll), (4, 3));
        app.on_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL));
        app.on_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!((app.selected, app.scroll), (2, 2));
        assert_eq!(
            app.shown(),
            [
                Shown {
                    command_id: 3,
                    rank: 3
                },
                Shown {
                    command_id: 4,
                    rank: 4
                }
            ]
        );
    }

    #[test]
    fn accept_and_cancel() {
        let (mut app, _rx) = app();
        assert_eq!(
            app.on_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Exit(None)
        );
        app.on_worker(updated(1, rows(3)));
        app.on_key(press(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(
            app.on_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Exit(Some(Arc::from("c1")))
        );
        for cancel in [
            press(KeyCode::Esc, KeyModifiers::NONE),
            press(KeyCode::Char('c'), KeyModifiers::CONTROL),
            press(KeyCode::Char('d'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(app.on_key(cancel), Flow::Exit(None));
        }
    }

    #[test]
    fn details_and_pages_are_requested_once() {
        let (mut app, rx) = app();
        let mut response = updated(1, rows(40));
        if let CmdResponse::QueryUpdated { total_matched, .. } = &mut response {
            *total_matched = 500;
        }
        app.on_worker(response);
        app.request_after_draw();
        app.request_after_draw();
        assert_eq!(requests(&rx), ["Details(1)", "GetRows(1, 40)"]);
        app.on_worker(CmdResponse::Details(CmdDetails {
            command_id: 1,
            dirs: Vec::new(),
            preceded_by: Vec::new(),
        }));
        app.request_after_draw();
        assert!(requests(&rx).is_empty());
    }

    #[test]
    fn status_lasts_until_the_next_key_and_failures_set_it() {
        let (mut app, _rx) = app();
        app.on_worker(CmdResponse::LoadFailed {
            message: "loading commands failed: database busy".to_string(),
        });
        assert_eq!(
            app.status.as_deref(),
            Some("Could not read history: loading commands failed: database busy")
        );
        app.on_key(plain('a'));
        assert_eq!(app.status, None);
        assert_eq!(app.query, "a", "the key is then handled normally");
        app.on_worker(CmdResponse::DeleteFailed);
        assert_eq!(app.status.as_deref(), Some("Could not delete"));
    }

    #[test]
    fn e_b_04_a_dead_worker_freezes_the_list_but_enter_still_picks() {
        let (mut app, rx) = app();
        app.on_worker(updated(1, rows(2)));
        drop(rx);
        app.on_key(plain('q'));
        assert_eq!(app.status.as_deref(), Some("History worker stopped"));
        assert_eq!(app.rows.len(), 2);
        assert_eq!(
            app.on_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Exit(Some(Arc::from("c0")))
        );
    }

    #[test]
    fn e_b_06_a_pick_that_cannot_be_logged_is_still_printed() {
        let store = CmdStore::in_memory();
        crate::cmd::store::test_support::execute(&store, "DROP TABLE command_events");
        let (tx, _rx) = mpsc::channel();
        let place = PickPlace {
            cwd: "/w".to_string(),
            repo_root: None,
            shell_session: None,
        };
        let events = CmdEvents::new("tui".to_string(), place, Some(store));
        let mut app = CmdApp::new(
            tx,
            events,
            String::new(),
            1_000_000,
            true,
            None,
            "/w".to_string(),
        );
        app.on_worker(updated(1, rows(2)));
        app.set_visible_rows(10);
        assert_eq!(
            app.on_key(press(KeyCode::Enter, KeyModifiers::NONE)),
            Flow::Exit(Some(Arc::from("c0")))
        );
    }

    #[test]
    fn the_clock_steps_to_whole_minutes_and_never_back() {
        let (mut app, _rx) = app();
        // 1_000_000 is 40 s past a minute.
        app.on_tick(1_000_015);
        assert_eq!(
            app.list_clock, 1_000_000,
            "the start time until the next minute"
        );
        app.on_tick(1_000_030);
        assert_eq!(app.list_clock, 1_000_020);
        app.on_tick(1_000_125);
        assert_eq!(app.list_clock, 1_000_080);
        app.on_tick(999_000);
        assert_eq!(app.list_clock, 1_000_080);
    }

    #[test]
    fn cwd_comes_from_the_flag_then_an_absolute_pwd() {
        assert_eq!(resolve_cwd(Some("rel".into()), Some("/p".into())), "rel");
        assert_eq!(resolve_cwd(None, Some("/p".into())), "/p");
        let current = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(resolve_cwd(None, Some("relative".into())), current);
        assert_eq!(resolve_cwd(None, None), current);
    }
}
