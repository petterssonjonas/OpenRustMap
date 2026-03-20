//! Restores the terminal (raw mode, mouse capture, alternate screen) on normal exit,
//! panic, or drop — avoids leaving the shell unusable after a crash.

use crossterm::cursor::Show;
use crossterm::event::DisableMouseCapture;
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};
use crossterm::ExecutableCommand;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static TERMINAL_RESTORED: AtomicBool = AtomicBool::new(false);

/// Idempotent: safe from Drop, panic hook, and explicit call.
pub fn restore_terminal() {
    if TERMINAL_RESTORED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = disable_raw_mode();
    let _ = out.execute(DisableMouseCapture);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = out.execute(Show);
    let _ = out.flush();
}

/// Install once: chain to the previous panic hook after restoring the terminal.
pub fn install_panic_hook() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
    });
}

/// RAII: call [`restore_terminal`] on drop (normal exit).
pub struct TerminalRestoreGuard;

impl TerminalRestoreGuard {
    pub fn new() -> Self {
        Self
    }
}

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}
