//! Terminal setup and teardown (raw mode, alternate screen), restored on exit, error and panic. While the
//! screen is ours, stderr goes to a log file so log lines cannot corrupt it.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange, EnableMouseCapture};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
use ratatui::Terminal;

pub type Term = Terminal<CrosstermBackend<io::Stdout>>;

// Globals because the panic hook has no handle to the terminal state.
/// The original stderr, duplicated before redirecting to the log; -1 if not redirected.
static STDERR: AtomicI32 = AtomicI32::new(-1);
/// Whether `enter` ran and `leave` has not.
static ON: AtomicBool = AtomicBool::new(false);
/// Verbose logging (`--debug` or NORI_DEBUG=1), process-wide like a logger level.
static DEBUG: AtomicBool = AtomicBool::new(false);

pub fn set_debug(on: bool) {
    DEBUG.store(on, Ordering::Relaxed);
}

pub fn debugging() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// Logs a line to nori.log when debugging; the message is formatted only then.
macro_rules! debug {
    ($($t:tt)*) => {
        if $crate::term::debugging() {
            eprintln!("nori debug {:?}: {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() % 100_000), format!($($t)*));
        }
    };
}
pub(crate) use debug;

/// Redirects stderr to `log` (truncated), keeping the original for panic messages.
pub fn stderr_to(log: &Path) {
    let Ok(file) = std::fs::File::create(log) else { return };
    // SAFETY: plain descriptor calls on descriptors this process owns; the file stays open as fd 2.
    unsafe {
        let saved = libc::dup(2);
        if saved >= 0 && libc::dup2(file.as_raw_fd(), 2) >= 0 {
            STDERR.store(saved, Ordering::SeqCst);
        }
    }
}

/// Writes to the original stderr.
fn to_terminal(text: &str) {
    let fd: RawFd = match STDERR.load(Ordering::SeqCst) {
        -1 => 2,
        fd => fd,
    };
    // SAFETY: a write to a descriptor this process owns.
    unsafe {
        libc::write(fd, text.as_ptr().cast(), text.len());
    }
}

/// Enables raw mode, the alternate screen, bracketed paste, focus reports and optionally the mouse.
pub fn enter(mouse: bool) -> io::Result<Term> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    // Focus reports let the runner redraw everything, pictures included, when the terminal comes back.
    execute!(out, EnterAlternateScreen, EnableBracketedPaste, EnableFocusChange, cursor::Hide)?;
    ON.store(true, Ordering::SeqCst);
    set_mouse(mouse);
    let mut t = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    t.clear()?;
    Ok(t)
}

/// Makes the next draw write every cell. Not `Terminal::clear`: it queries the cursor position, and the
/// input thread would eat the reply as a key. A blank frame, then an erase, so the next diff is complete.
pub fn repaint(t: &mut Term) -> io::Result<()> {
    use ratatui::backend::Backend;
    t.draw(|_| {})?;
    t.backend_mut().clear_region(ratatui::backend::ClearType::All)?;
    Ok(())
}

/// Whether the program runs inside tmux.
pub fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// A tmux command's answer, if tmux ran.
fn tmux(args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("tmux").args(args).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string())
}

/// Whether tmux itself renders sixel: only when its client terminal has the `sixel` feature. tmux claims
/// sixel support regardless, and on a terminal without it shows a "SIXEL IMAGE" placeholder box.
pub fn tmux_draws_sixel() -> bool {
    let features = tmux(&["display", "-p", "#{client_termfeatures}"]);
    eprintln!("nori: tmux's terminal features: {}", features.as_deref().unwrap_or("(tmux did not say)"));
    features.is_some_and(|f| sixel_feature(&f))
}

/// Whether `sixel` is in tmux's comma-separated terminal features.
pub fn sixel_feature(features: &str) -> bool {
    features.split(',').any(|f| f.trim() == "sixel")
}

/// Runs `f` with TERM/TERM_PROGRAM hiding tmux, so ratatui-image queries tmux itself instead of passing
/// through. Must run before other threads read the environment. tmux takes the kitty query (an APC
/// sequence) as a pane title, so the title is restored afterwards.
pub fn not_tmux<R>(f: impl FnOnce() -> R) -> R {
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let title = tmux(&["display", "-p", "-t", &pane, "#{pane_title}"]);
    let (term, program) = (std::env::var_os("TERM"), std::env::var_os("TERM_PROGRAM"));
    std::env::set_var("TERM", "xterm-256color");
    std::env::remove_var("TERM_PROGRAM");
    let r = f();
    match term {
        Some(t) => std::env::set_var("TERM", t),
        None => std::env::remove_var("TERM"),
    }
    if let Some(p) = program {
        std::env::set_var("TERM_PROGRAM", p);
    }
    if let Some(t) = title {
        tmux(&["select-pane", "-t", &pane, "-T", &t]);
    }
    r
}

/// Turns on tmux `focus-events`. Passed-through pictures are not kept by tmux, so they must be resent when
/// the pane's window returns, and tmux only reports that with focus-events on.
pub fn tmux_focus_events() {
    if !in_tmux() {
        return;
    }
    if tmux(&["show", "-sv", "focus-events"]).is_some_and(|v| v.trim() != "on") {
        let set = tmux(&["set", "-s", "focus-events", "on"]).is_some();
        eprintln!("nori: tmux focus-events switched on ({set}), so covers are sent again when the window comes back");
    }
}

/// Mouse capture on or off; off lets the terminal select text.
pub fn set_mouse(on: bool) {
    let mut out = io::stdout();
    let _ = if on { execute!(out, EnableMouseCapture) } else { execute!(out, DisableMouseCapture) };
}

/// Undoes [`enter`]. Idempotent; called from the panic hook too.
pub fn leave() {
    if !ON.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = execute!(out, DisableMouseCapture, DisableFocusChange, DisableBracketedPaste, LeaveAlternateScreen, cursor::Show);
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Restores the terminal on a main-thread panic and prints the message to the original stderr.
pub fn hook_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Worker panics (e.g. a cover that fails to decode, caught by the loader) are only logged.
        if std::thread::current().name() == Some("main") {
            leave();
            to_terminal(&format!("nori: {info}\n"));
        }
        default(info);
    }));
}
