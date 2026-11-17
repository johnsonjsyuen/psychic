//! Drawing the picker (`docs/ctrl-r/02-browse.md` section 8.3).
//!
//! [`draw`] is pure: it reads `&CmdApp` and nothing else - no I/O, no clock
//! (time-ago uses `app.list_clock`), no environment (`~` uses `app.home`).
//! That keeps a redraw free of syscalls (B5).

use super::app::CmdApp;
use super::features::COMMAND_FEATURE_REGISTRY;
use super::keys::{CMD_KEYMAP, keys_of};
use super::model::ml_weight;
use super::tuning::CMD_STACK_BELOW_COLS;
use super::worker::CmdRow;
use crate::feature_defs::schema::FeatureType;
use crate::path_display::{display_width, get_time_ago, printable, truncate_to_width};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// What the event loop needs to know about the frame just drawn.
pub(super) struct CmdLayout {
    pub visible_rows: usize,
}

/// Columns of a list row besides the command: rank, fail and here markers, time.
const RANK_COLS: usize = 4;
const FAIL_COLS: usize = 1;
const HERE_COLS: usize = 2;
const TIME_COLS: usize = 16;

/// Below this the panes do not fit and only the input box is drawn (E-B-12).
const MIN_COLS: u16 = 20;
const MIN_ROWS: u16 = 5;

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn label() -> Style {
    Style::default().fg(Color::Cyan)
}

/// A command as one list row: newlines shown as `↵ `, control characters
/// made safe, cut to `width` columns.
pub(super) fn row_text(text: &str, width: usize) -> String {
    let joined = text.replace('\n', "\u{21B5} ");
    truncate_to_width(&printable(&joined), width).into_owned()
}

/// `dir` with a leading `home` shown as `~`.
fn tilde(dir: &str, home: Option<&str>) -> String {
    match home.filter(|home| !home.is_empty() && *home != "/") {
        Some(home) if dir == home => "~".to_string(),
        Some(home) => match dir.strip_prefix(home) {
            Some(rest) if rest.starts_with('/') => format!("~{}", rest),
            _ => dir.to_string(),
        },
        None => dir.to_string(),
    }
}

fn exit_text(code: Option<i32>) -> String {
    match code {
        Some(0) => "0".to_string(),
        Some(code) => format!("{} (failed)", code),
        None => "unknown".to_string(),
    }
}

fn pad_to(text: &mut String, width: usize) {
    let used = display_width(text);
    if used < width {
        text.extend(std::iter::repeat_n(' ', width - used));
    }
}

/// Draw the picker; returns how many list rows fit.
pub(super) fn draw(f: &mut Frame, app: &CmdApp) -> CmdLayout {
    let area = f.area();
    if area.width < MIN_COLS || area.height < MIN_ROWS {
        let input = Rect {
            y: area.y + area.height.saturating_sub(3),
            height: area.height.min(3),
            ..area
        };
        draw_input(f, input, app);
        return CmdLayout { visible_rows: 0 };
    }
    let [top, input] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .areas(area);

    let wide = area.width >= CMD_STACK_BELOW_COLS;
    let (direction, percents): (Direction, &[u16]) = match (wide, app.debug_visible) {
        (true, false) => (Direction::Horizontal, &[50, 50]),
        (true, true) => (Direction::Horizontal, &[35, 30, 35]),
        (false, false) => (Direction::Vertical, &[60, 40]),
        (false, true) => (Direction::Vertical, &[40, 60]),
    };
    let panes = Layout::default()
        .direction(direction)
        .constraints(percents.iter().map(|&p| Constraint::Percentage(p)))
        .split(top);
    let list = panes[0];
    let (preview, debug) = match (wide, app.debug_visible) {
        (_, false) => (Some(panes[1]), None),
        (true, true) => (Some(panes[1]), Some(panes[2])),
        (false, true) => (None, Some(panes[1])),
    };

    let visible_rows = draw_list(f, list, app);
    if let Some(preview) = preview {
        draw_preview(f, preview, app);
    }
    if let Some(debug) = debug {
        draw_debug(f, debug, app);
    }
    draw_input(f, input, app);
    if app.help_visible {
        draw_help(f, area);
    }
    CmdLayout { visible_rows }
}

fn list_row(row: &CmdRow, rank: usize, width: usize, now: i64, selected: bool) -> Line<'static> {
    let command_width = width.saturating_sub(RANK_COLS + FAIL_COLS + HERE_COLS + TIME_COLS);
    let mut command = row_text(&row.text, command_width);
    pad_to(&mut command, command_width);
    let failed = matches!(row.last_exit_code, Some(code) if code != 0);
    let time = get_time_ago(Some(row.last_run_at), now);
    let time = truncate_to_width(&time, TIME_COLS).into_owned();
    let line = Line::from(vec![
        Span::styled(format!("{:>width$}", rank, width = RANK_COLS), dim()),
        if failed {
            Span::styled("!", Style::default().fg(Color::Red))
        } else {
            Span::raw(" ")
        },
        if row.runs_here > 0 {
            Span::styled("* ", dim())
        } else {
            Span::raw("  ")
        },
        Span::raw(command),
        Span::styled(format!("{:>width$}", time, width = TIME_COLS), dim()),
    ]);
    if selected {
        line.style(Style::default().add_modifier(Modifier::REVERSED))
    } else {
        line
    }
}

/// The list pane; returns how many rows fit in it.
fn draw_list(f: &mut Frame, area: Rect, app: &CmdApp) -> usize {
    let block = Block::default().borders(Borders::ALL).title("Commands");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let visible = inner.height as usize;
    if app.rows.is_empty() {
        if app.has_results() {
            let mut lines = vec![Line::from(if app.load_failed {
                "Could not read history."
            } else if app.total_commands == 0 {
                "No commands recorded yet."
            } else {
                "No match."
            })];
            if !app.recording_on && !app.load_failed {
                lines.push(Line::from(
                    "Recording is off. Turn it on with: psychic history --enable",
                ));
            }
            let top = inner.height.saturating_sub(lines.len() as u16) / 2;
            let message = Rect {
                y: inner.y + top,
                height: inner.height - top,
                ..inner
            };
            f.render_widget(
                Paragraph::new(lines)
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true }),
                message,
            );
        }
        return visible;
    }
    let end = (app.scroll + visible).min(app.rows.len());
    let lines: Vec<Line> = (app.scroll..end)
        .map(|i| {
            list_row(
                &app.rows[i],
                i + 1,
                inner.width as usize,
                app.list_clock,
                i == app.selected,
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    visible
}

fn field(name: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{:<11}", name), label()),
        Span::raw(value),
    ])
}

fn draw_preview(f: &mut Frame, area: Rect, app: &CmdApp) {
    let block = Block::default().borders(Borders::ALL).title("Preview");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(row) = app.selected_row() else {
        return;
    };
    let width = inner.width as usize;
    let mut lines: Vec<Line> = row
        .text
        .split('\n')
        .map(|line| Line::from(printable(line).into_owned()))
        .collect();
    lines.push(Line::default());
    lines.push(field(
        "Last run",
        get_time_ago(Some(row.last_run_at), app.list_clock),
    ));
    lines.push(field(
        "Runs",
        format!("{} here / {} total", row.runs_here, row.run_count),
    ));
    lines.push(field("Last exit", exit_text(row.last_exit_code)));
    if row.pick_count > 0 {
        lines.push(field(
            "Picked",
            format!("{} times with Ctrl-R", row.pick_count),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::styled("Last directories", label()));
    match app.details.get(&row.command_id) {
        None => lines.push(Line::from("  ...")),
        Some(details) => {
            for dir in &details.dirs {
                let suffix = format!(
                    "  {} runs  {}",
                    dir.runs,
                    get_time_ago(Some(dir.last_run_at), app.list_clock)
                );
                let room = width.saturating_sub(2 + display_width(&suffix));
                let path = tilde(&dir.dir, app.home.as_deref());
                let path = truncate_to_width(&printable(&path), room).into_owned();
                lines.push(Line::from(vec![
                    Span::raw(format!("  {}", path)),
                    Span::styled(suffix, dim()),
                ]));
            }
            if !details.preceded_by.is_empty() {
                lines.push(Line::default());
                lines.push(Line::styled("Usually after", label()));
                for before in &details.preceded_by {
                    let suffix = format!("  {} times", before.runs);
                    let room = width.saturating_sub(2 + display_width(&suffix));
                    let template = row_text(&before.template, room);
                    lines.push(Line::from(vec![
                        Span::raw(format!("  {}", template)),
                        Span::styled(suffix, dim()),
                    ]));
                }
            }
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn optional(value: Option<f64>, digits: usize) -> String {
    value.map_or_else(|| "n/a".to_string(), |v| format!("{:.*}", digits, v))
}

fn draw_debug(f: &mut Frame, area: Rect, app: &CmdApp) {
    let block = Block::default().borders(Borders::ALL).title("Debug");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    if let Some(row) = app.selected_row() {
        let parts = &row.heuristic_parts;
        lines.push(field("score", format!("{:.4}", row.score)));
        lines.push(field("heuristic", format!("{:.4}", row.heuristic)));
        lines.push(field("  fzf", parts.fzf.to_string()));
        lines.push(field("  recency", format!("{:.4}", parts.recency)));
        lines.push(field("ml_score", optional(row.ml_score, 4)));
        lines.push(field("ml_weight", format!("{:.2}", row.ml_weight)));
        lines.push(field(
            "fuzzy_score",
            row.fuzzy_score
                .map_or_else(|| "-".to_string(), |s| s.to_string()),
        ));
        if !row.features.is_empty() {
            for (feature, value) in COMMAND_FEATURE_REGISTRY.iter().zip(&row.features) {
                let value = match feature.feature_type() {
                    _ if value.is_nan() => "n/a".to_string(),
                    FeatureType::Binary => format!("{:.0}", value),
                    FeatureType::Numeric => format!("{:.4}", value),
                };
                lines.push(Line::from(format!("{} = {}", feature.name(), value)));
            }
        } else if app.model.is_some() {
            lines.push(Line::from("not model-scored (rank beyond stage 2)"));
        }
        lines.push(Line::default());
    }
    match &app.model {
        None => lines.push(Line::from("model: none")),
        Some(model) => {
            lines.push(Line::from(format!(
                "model: {} trees, {} groups, ml weight {:.2}",
                model.best_iteration,
                model.groups(),
                ml_weight(model.blend_examples)
            )));
            lines.push(Line::from(format!("trained {}", model.trained_at)));
            for gain in model.feature_gain.iter().take(3) {
                lines.push(Line::from(format!("  {} {:.1}", gain.feature, gain.gain)));
            }
        }
    }
    lines.push(Line::default());
    lines.push(field(
        "here",
        printable(&tilde(&app.cwd, app.home.as_deref())).into_owned(),
    ));
    lines.push(field(
        "load",
        match &app.loaded {
            Some(loaded) => format!("{:.1} ms, {} commands", loaded.load_ms, loaded.commands),
            None => "n/a".to_string(),
        },
    ));
    lines.push(field(
        "first results",
        format!("{} ms", optional(app.timings.first_results_ms, 1)),
    ));
    lines.push(field(
        "rank",
        format!("{} ms", optional(app.timings.last_rank_ms, 2)),
    ));
    lines.push(field(
        "round trip",
        format!("{} ms", optional(app.timings.last_round_trip_ms, 2)),
    ));
    lines.push(field(
        "matched",
        format!("{} of {}", app.total_matched, app.total_commands),
    ));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_input(f: &mut Frame, area: Rect, app: &CmdApp) {
    let title = match &app.status {
        Some(status) => Span::styled(
            format!(" {} ", printable(status)),
            Style::default().fg(Color::Yellow),
        ),
        None => Span::raw(format!(
            " history - {} - {}/{} ",
            app.scope.label(),
            app.total_matched,
            app.total_commands
        )),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(Line::styled(" Ctrl-G help ", dim()).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    let content = if app.confirm_delete.is_some() {
        "Delete this command from psychic's history? y = delete, any other key = keep".to_string()
    } else {
        format!("> {}", printable(&app.query))
    };
    f.render_widget(Paragraph::new(content.clone()), inner);
    if app.confirm_delete.is_none() && !app.help_visible && inner.width > 0 && inner.height > 0 {
        let x = inner.x + (display_width(&content) as u16).min(inner.width - 1);
        f.set_cursor_position((x, inner.y));
    }
}

fn draw_help(f: &mut Frame, area: Rect) {
    let keys: Vec<String> = CMD_KEYMAP.iter().map(keys_of).collect();
    let key_width = keys.iter().map(|k| display_width(k)).max().unwrap_or(0);
    let mut lines: Vec<Line> = CMD_KEYMAP
        .iter()
        .zip(&keys)
        .map(|(binding, keys)| {
            Line::from(vec![
                Span::styled(format!("{:<width$}  ", keys, width = key_width), label()),
                Span::raw(binding.description),
            ])
        })
        .collect();
    lines.push(Line::default());
    lines.push(Line::from(
        "Ctrl-R in your shell opens this; Enter puts the command on your prompt.",
    ));
    let content_width = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let width = (content_width + 4).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Keys ")),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::app::test_support::{app, row, updated};
    use crate::cmd::store::{DirUse, PrecededBy};
    use crate::cmd::worker::{CmdDetails, CmdResponse};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn tc_b_13_row_text() {
        assert_eq!(
            row_text("echo a\nb\tc\x1b[31m", 40),
            "echo a\u{21B5} b    c\u{B7}[31m"
        );
        assert_eq!(row_text("echo a\nb\tc\x1b[31m", 6), "echo \u{2026}");
        // CJK is two columns a character; the cut is on a character boundary.
        assert_eq!(row_text("echo 日本語のテキスト", 10), "echo 日本\u{2026}");
    }

    // TC-N-10 (B5): render functions make no system or SQLite calls.
    #[test]
    fn tc_n_10_render_is_pure() {
        let source = include_str!("render.rs");
        let code = &source[..source
            .find("#[cfg(test)]\nmod tests")
            .expect("the test module is last")];
        for banned in [
            "rusqlite",
            "std::fs",
            "std::env",
            "Instant::now",
            "Timestamp::now",
            "std::process",
            "log::",
        ] {
            assert!(!code.contains(banned), "render.rs uses {banned}");
        }
    }

    #[test]
    fn tilde_shortens_only_the_home_directory_itself() {
        assert_eq!(tilde("/home/u", Some("/home/u")), "~");
        assert_eq!(tilde("/home/u/x", Some("/home/u")), "~/x");
        assert_eq!(tilde("/home/uv", Some("/home/u")), "/home/uv");
        assert_eq!(tilde("/x", None), "/x");
        assert_eq!(tilde("/x", Some("/")), "/x");
    }

    fn screen(width: u16, height: u16, app: &CmdApp) -> (Vec<String>, usize) {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut visible = 0;
        terminal
            .draw(|f| visible = draw(f, app).visible_rows)
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let lines = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        (lines, visible)
    }

    fn find(lines: &[String], text: &str) -> Option<usize> {
        lines.iter().position(|line| line.contains(text))
    }

    /// IT-B-04: three rows (one failed, one run here, one multi-line).
    fn three_rows() -> CmdApp {
        let (mut app, _rx) = app();
        let mut failed = row(1, "make test");
        failed.last_exit_code = Some(2);
        let mut here = row(2, "cargo build");
        here.runs_here = 2;
        let multi = row(3, "echo 'one\ntwo'");
        app.on_worker(updated(1, vec![failed, here, multi]));
        app.on_worker(CmdResponse::Details(CmdDetails {
            command_id: 1,
            dirs: vec![DirUse {
                dir: "/home/u/src".to_string(),
                runs: 4,
                last_run_at: 999_000,
            }],
            preceded_by: vec![PrecededBy {
                template: "git pull".to_string(),
                runs: 3,
            }],
        }));
        app
    }

    #[test]
    fn it_b_04_wide_terminals_put_the_preview_beside_the_list() {
        let mut app = three_rows();
        let (lines, visible) = screen(140, 30, &app);
        let list_title = find(&lines, "Commands").unwrap();
        let preview_title = find(&lines, "Preview").unwrap();
        assert_eq!(
            list_title,
            preview_title,
            "side by side:\n{}",
            lines.join("\n")
        );
        assert_eq!(visible, 30 - 3 - 2);

        let failed = find(&lines, "make test").unwrap();
        assert!(
            lines[failed].contains("   1!  make test"),
            "{}",
            lines[failed]
        );
        let here = find(&lines, "cargo build").unwrap();
        assert!(
            lines[here].contains("   2 * cargo build"),
            "{}",
            lines[here]
        );
        assert!(find(&lines, "echo 'one\u{21B5} two'").is_some());
        assert!(
            lines[failed].contains("16 minutes ago"),
            "{}",
            lines[failed]
        );

        assert!(find(&lines, "Runs       0 here / 3 total").is_some());
        assert!(find(&lines, "Last exit  2 (failed)").is_some());
        assert!(find(&lines, "~/src  4 runs  16 minutes ago").is_some());
        assert!(find(&lines, "git pull  3 times").is_some());
        assert!(find(&lines, " history - all - 3/3 ").is_some());
        assert!(find(&lines, "Ctrl-G help").is_some());
        assert!(find(&lines, "> ").is_some());

        app.debug_visible = true;
        let (lines, _) = screen(140, 30, &app);
        assert!(find(&lines, "model: none").is_some());
        assert_eq!(find(&lines, "Debug"), find(&lines, "Preview"));
    }

    #[test]
    fn it_b_04_narrow_terminals_stack_the_list_over_the_preview() {
        let mut app = three_rows();
        let (lines, _) = screen(100, 30, &app);
        let list_title = find(&lines, "Commands").unwrap();
        let preview_title = find(&lines, "Preview").unwrap();
        assert!(preview_title > list_title, "stacked:\n{}", lines.join("\n"));

        app.debug_visible = true;
        let (lines, _) = screen(100, 30, &app);
        assert!(find(&lines, "Preview").is_none(), "preview hidden");
        assert!(find(&lines, "model: none").is_some());
    }

    #[test]
    fn it_b_04_help_lists_every_key_row() {
        let mut app = three_rows();
        app.help_visible = true;
        let (lines, _) = screen(140, 30, &app);
        for binding in CMD_KEYMAP {
            assert!(
                find(&lines, binding.description).is_some(),
                "{} missing:\n{}",
                binding.description,
                lines.join("\n")
            );
        }
        assert_eq!(CMD_KEYMAP.len(), 12);
        assert!(find(&lines, "Ctrl-R in your shell opens this").is_some());
    }

    #[test]
    fn empty_lists_say_why() {
        let (mut app, _rx) = app();
        let (lines, _) = screen(140, 30, &app);
        assert!(
            find(&lines, "No commands").is_none(),
            "nothing before the first answer"
        );
        app.on_worker(updated(1, Vec::new()));
        app.recording_on = false;
        let (lines, _) = screen(140, 30, &app);
        assert!(find(&lines, "No commands recorded yet.").is_some());
        assert!(
            find(
                &lines,
                "Recording is off. Turn it on with: psychic history --enable"
            )
            .is_some()
        );

        app.recording_on = true;
        app.total_commands = 5;
        let (lines, _) = screen(140, 30, &app);
        assert!(find(&lines, "No match.").is_some());
        assert!(find(&lines, "Recording is off").is_none());
    }

    #[test]
    fn e_b_01_a_history_that_could_not_be_read_says_so_after_the_status_is_gone() {
        let (mut app, _rx) = app();
        app.recording_on = false;
        app.on_worker(CmdResponse::LoadFailed {
            message: "loading commands failed: database busy".to_string(),
        });
        app.on_worker(updated(1, Vec::new()));
        app.status = None;
        let (lines, _) = screen(140, 30, &app);
        assert!(find(&lines, "Could not read history.").is_some());
        assert!(find(&lines, "No commands recorded yet.").is_none());
        assert!(find(&lines, "Recording is off").is_none());
    }

    #[test]
    fn status_and_confirmation_replace_the_title_and_the_query() {
        let mut app = three_rows();
        app.status = Some("Could not delete".to_string());
        app.confirm_delete = Some(1);
        let (lines, _) = screen(140, 30, &app);
        assert!(find(&lines, "Could not delete").is_some());
        assert!(find(&lines, " history - ").is_none());
        assert!(
            find(
                &lines,
                "Delete this command from psychic's history? y = delete"
            )
            .is_some()
        );
    }

    #[test]
    fn e_b_12_a_tiny_terminal_gets_only_the_input_box() {
        let app = three_rows();
        let (lines, visible) = screen(15, 4, &app);
        assert_eq!(visible, 0);
        assert!(find(&lines, "Commands").is_none());
        let (_, visible) = screen(40, 4, &app);
        assert_eq!(visible, 0);
    }
}
