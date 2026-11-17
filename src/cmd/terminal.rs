//! The picker's terminal: set up once, and given back on every way out
//! (`docs/ctrl-r/02-browse.md` section 4 steps 5 and 8, E-B-03, E-B-20,
//! E-B-21).
//!
//! The ways out, and what gives the terminal back on each:
//!
//! | Way out | Restored by |
//! |---|---|
//! | Enter, Esc, an `Err` from the event loop | [`PickerTerminal::leave`] |
//! | An early `?` or a panic that unwinds past the UI thread's frames | `Drop` of [`PickerTerminal`] |
//! | A panic on the UI thread | the hook of [`restore_on_panic`], before the message prints |
//! | SIGTERM, SIGHUP, SIGINT | [`TerminationSignals`] turns them into an event; the loop ends and `leave` runs |
//!
//! Restoring twice would pop the terminal's keyboard-flag stack twice (the
//! second pop takes an entry that is not ours), so one process-wide flag
//! says whether the terminal is still set up, and whoever clears it restores.

use anyhow::{Context, Result};
use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::os::raw::c_int;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The picker's terminal. Buffered: a frame reaches the terminal as one
/// write when ratatui flushes it, not one write per escape sequence (an
/// unbuffered `/dev/tty` took about 6 ms of syscalls per redraw of the list).
pub(super) type Tui = Terminal<CrosstermBackend<std::io::BufWriter<std::fs::File>>>;

/// Big enough for a full 40 x 120 frame of styled cells.
const FRAME_BUFFER_BYTES: usize = 64 * 1024;

/// Set while raw mode, the alternate screen, mouse capture and the pushed
/// keyboard flags are in force. Process-wide because the panic hook cannot
/// reach the `Terminal`.
static SET_UP: AtomicBool = AtomicBool::new(false);

/// Take the job of restoring: `true` for exactly one caller per set-up.
fn take_restore() -> bool {
    SET_UP.swap(false, Ordering::SeqCst)
}

/// The set-up terminal; gives it back when dropped, if nothing did before.
pub(super) struct PickerTerminal {
    tui: Tui,
}

impl PickerTerminal {
    /// Raw mode, alternate screen, mouse, key disambiguation: the sequence of
    /// file mode, so `input::leave_tui` undoes it exactly. A failure midway
    /// undoes what was done (E-B-03).
    pub(super) fn enter() -> Result<Self> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("no terminal to draw on (/dev/tty)")?;
        let mut cleanup = tty
            .try_clone()
            .context("no terminal to draw on (/dev/tty)")?;
        enable_raw_mode().context("could not set up the terminal")?;
        let set_up = (|| -> Result<Tui> {
            execute!(tty, EnterAlternateScreen)?;
            execute!(tty, crossterm::event::EnableMouseCapture)?;
            execute!(
                tty,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            let tty = std::io::BufWriter::with_capacity(FRAME_BUFFER_BYTES, tty);
            Ok(Terminal::new(CrosstermBackend::new(tty))?)
        })();
        match set_up {
            Ok(tui) => {
                SET_UP.store(true, Ordering::SeqCst);
                Ok(PickerTerminal { tui })
            }
            Err(error) => {
                undo_setup(&mut cleanup);
                Err(error.context("could not set up the terminal"))
            }
        }
    }

    /// Give the terminal back now, on the way out of a normal exit. Does
    /// nothing if the panic hook already did.
    pub(super) fn leave(&mut self) -> Result<()> {
        if take_restore() {
            crate::input::leave_tui(&mut self.tui)?;
        }
        Ok(())
    }
}

impl std::ops::Deref for PickerTerminal {
    type Target = Tui;
    fn deref(&self) -> &Tui {
        &self.tui
    }
}

impl std::ops::DerefMut for PickerTerminal {
    fn deref_mut(&mut self) -> &mut Tui {
        &mut self.tui
    }
}

impl Drop for PickerTerminal {
    fn drop(&mut self) {
        if take_restore() {
            let _ = crate::input::leave_tui(&mut self.tui);
        }
    }
}

/// `input::leave_tui`'s sequence on a terminal that is not a `Terminal`:
/// a set-up that failed midway, or the panic hook.
fn undo_setup(tty: &mut std::fs::File) {
    use crossterm::cursor::Show;
    use crossterm::event::{DisableMouseCapture, PopKeyboardEnhancementFlags};
    use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
    let _ = disable_raw_mode();
    let _ = execute!(
        tty,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show
    );
}

/// From the next panic on the calling thread (the UI thread) on: give the
/// terminal back before the panic message prints, so the message lands on
/// the main screen in cooked mode instead of vanishing with the alternate
/// screen. Panics on other threads (the worker's are handled, E-B-04) leave
/// the terminal alone. Installed once per process; the previous hook still
/// prints the message.
pub(super) fn restore_on_panic() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    let ui_thread = std::thread::current().id();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == ui_thread && take_restore() {
                // The `Terminal` and its buffered writer are out of reach;
                // the same sequence goes to a fresh handle.
                if let Ok(mut tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
                    undo_setup(&mut tty);
                }
            }
            previous(info);
        }));
    });
}

/// The signals that end the picker: `kill`'s default, the terminal going
/// away, and an interrupt sent from outside (in raw mode Ctrl-C is a key).
const TERMINATING: [c_int; 3] = [
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGHUP,
    signal_hook::consts::SIGINT,
];

/// While alive, SIGTERM, SIGHUP and SIGINT do not kill the process at once:
/// the first one is handed to a callback (the picker sends it to its event
/// loop, which leaves through the normal path and then re-raises it), and a
/// second one of the same kind takes the default action, so a picker stuck
/// on its way out can still be killed. Once it is dropped, the next one takes
/// the default action too.
///
/// The handlers only set a flag and write a byte to a socket (both
/// async-signal-safe, done by `signal_hook`); everything else runs on a
/// thread of its own.
pub(super) struct TerminationSignals {
    /// Per signal, whether the next one takes the default action.
    armed: Vec<Arc<AtomicBool>>,
}

impl TerminationSignals {
    pub(super) fn watch(on_signal: impl FnOnce(c_int) + Send + 'static) -> Result<Self> {
        const OP: &str = "could not watch for termination signals";
        let (mut wake_rx, wake_tx) = std::os::unix::net::UnixStream::pair().context(OP)?;
        let mut signals = TerminationSignals { armed: Vec::new() };
        let mut raised = Vec::new();
        for signal in TERMINATING {
            let flag = Arc::new(AtomicBool::new(false));
            // Kept in `signals` from the start, so an error below still arms
            // what was registered. The conditional default goes first: it
            // sees the flag before this signal's own registration sets it,
            // so only a second signal acts.
            signals.armed.push(Arc::clone(&flag));
            signal_hook::flag::register_conditional_default(signal, Arc::clone(&flag))
                .context(OP)?;
            signal_hook::flag::register(signal, Arc::clone(&flag)).context(OP)?;
            signal_hook::low_level::pipe::register(signal, wake_tx.try_clone().context(OP)?)
                .context(OP)?;
            raised.push((signal, flag));
        }
        drop(wake_tx);
        std::thread::Builder::new()
            .name("signals".to_string())
            .spawn(move || {
                use std::io::Read;
                let first = || {
                    raised
                        .iter()
                        .find(|(_, flag)| flag.load(Ordering::SeqCst))
                        .map(|&(signal, _)| signal)
                };
                let mut byte = [0u8; 1];
                let signal = loop {
                    match wake_rx.read(&mut byte) {
                        Ok(1..) => {
                            if let Some(signal) = first() {
                                break Some(signal);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        // Not expected: the handlers keep the write ends.
                        Ok(0) | Err(_) => break None,
                    }
                };
                if let Some(signal) = signal {
                    on_signal(signal);
                }
            })
            .context(OP)?;
        Ok(signals)
    }
}

/// The handlers stay installed (`signal_hook` cannot put the default
/// disposition back, and an unregistered SIGTERM would be ignored); arming
/// every flag makes the next signal take its default action instead.
impl Drop for TerminationSignals {
    fn drop(&mut self) {
        for flag in &self.armed {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// End the process as `signal` would have, now that the terminal is back.
pub(super) fn die_of(signal: c_int) -> ! {
    log::logger().flush();
    let _ = signal_hook::low_level::emulate_default_handler(signal);
    // Not reached for the three signals above; a fallback all the same.
    std::process::exit(128 + signal);
}
