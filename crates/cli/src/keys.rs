//! The key binding table, used by both dispatch and the help overlay (`?`).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Panel;

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Help,
    TogglePlay,
    Next,
    Previous,
    SeekBack,
    SeekForward,
    SeekBackLong,
    SeekForwardLong,
    VolumeUp,
    VolumeDown,
    Search,
    /// Focus cycles sidebar, page, panel.
    NextPane,
    PreviousPane,
    Mouse,
    Images,
    Shuffle,
    Repeat,
    /// Index into `app::GO`.
    Go(u8),
    /// Show that right panel; the same again hides it.
    Panel(Panel),
    /// Full-window player.
    Full,
    Back,
    Refresh,
    // Lists
    Up,
    Down,
    Top,
    Bottom,
    PageUp,
    PageDown,
    Open,
    Enqueue,
    PlayNext,
    Download,
    Star,
    PlayAll,
    ShuffleAll,
    // Card grids
    Left,
    Right,
    // Queue, downloads and equalizer band editing
    Remove,
    Undo,
    MoveUp,
    MoveDown,
    // Lyrics
    Sooner,
    Later,
    Unnudge,
    // Settings and equalizer values
    Decrease,
    Increase,
    // Previous or next settings group
    GroupBack,
    GroupOn,
}

/// Where a binding applies. Lookup goes from the focused part's scope to List to Global.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    List,
    Grid,
    Edit,
    Lyrics,
    Eq,
    Values,
}

impl Scope {
    /// Help overlay order.
    pub const ALL: [Scope; 7] = [Scope::Global, Scope::List, Scope::Grid, Scope::Edit, Scope::Lyrics, Scope::Eq, Scope::Values];

    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "Everywhere",
            Scope::List => "Lists and pages",
            Scope::Grid => "Albums, as cards",
            Scope::Edit => "The queue (and downloads)",
            Scope::Lyrics => "Lyrics",
            Scope::Eq => "Equalizer",
            Scope::Values => "Settings",
        }
    }
}

pub struct Binding {
    pub scope: Scope,
    pub keys: &'static [(KeyCode, KeyModifiers)],
    /// Help text for the keys; empty continues the row above.
    pub label: &'static str,
    pub action: Action,
    pub help: &'static str,
}

const N: KeyModifiers = KeyModifiers::NONE;
const S: KeyModifiers = KeyModifiers::SHIFT;
const C: KeyModifiers = KeyModifiers::CONTROL;

macro_rules! b {
    ($scope:ident, $label:expr, $action:expr, $help:expr, [$($k:expr),*]) => {
        Binding { scope: Scope::$scope, keys: &[$($k),*], label: $label, action: $action, help: $help }
    };
}

use KeyCode::*;

pub const BINDINGS: &[Binding] = &[
    b!(Global, "space", Action::TogglePlay, "Play or pause", [(Char(' '), N)]),
    b!(Global, "n / p", Action::Next, "Next song, previous (or the start of this one)", [(Char('n'), N)]),
    b!(Global, "", Action::Previous, "", [(Char('p'), N)]),
    b!(Global, "← → / , .", Action::SeekBack, "Back or forward 5 seconds (in a grid ← → move: , . seek)", [(Left, N), (Char(','), N)]),
    b!(Global, "", Action::SeekForward, "", [(Right, N), (Char('.'), N)]),
    b!(Global, "shift ← → / < >", Action::SeekBackLong, "Back or forward 30 seconds", [(Left, S), (Char('<'), N), (Char('<'), S)]),
    b!(Global, "", Action::SeekForwardLong, "", [(Right, S), (Char('>'), N), (Char('>'), S)]),
    b!(Global, "+ / -", Action::VolumeUp, "Volume up or down", [(Char('+'), N), (Char('+'), S), (Char('='), N)]),
    b!(Global, "", Action::VolumeDown, "", [(Char('-'), N)]),
    b!(Global, "s", Action::Shuffle, "Shuffle the queue on or off", [(Char('s'), N)]),
    b!(Global, "r", Action::Repeat, "Repeat: off, all, one", [(Char('r'), N)]),
    b!(Global, "/", Action::Search, "Search", [(Char('/'), N)]),
    b!(Global, "1 … 7", Action::Go(0), "Home, Albums, Artists, Songs, Downloads, Equalizer, Settings", [(Char('1'), N)]),
    b!(Global, "", Action::Go(1), "", [(Char('2'), N)]),
    b!(Global, "", Action::Go(2), "", [(Char('3'), N)]),
    b!(Global, "", Action::Go(3), "", [(Char('4'), N)]),
    b!(Global, "", Action::Go(4), "", [(Char('5'), N)]),
    b!(Global, "", Action::Go(5), "", [(Char('6'), N)]),
    b!(Global, "", Action::Go(6), "", [(Char('7'), N)]),
    b!(Global, "tab / shift tab", Action::NextPane, "Sidebar, page, panel", [(Tab, N)]),
    b!(Global, "", Action::PreviousPane, "", [(BackTab, S), (BackTab, N)]),
    b!(Global, "N / Q / L", Action::Panel(Panel::Playing), "The panel on the right: now playing, queue, lyrics (again: hide it)", [(Char('N'), S), (Char('N'), N)]),
    b!(Global, "", Action::Panel(Panel::Queue), "", [(Char('Q'), S), (Char('Q'), N)]),
    b!(Global, "", Action::Panel(Panel::Lyrics), "", [(Char('L'), S), (Char('L'), N)]),
    b!(Global, "F", Action::Full, "The player over the whole window", [(Char('F'), S), (Char('F'), N)]),
    b!(Global, "esc / h / ⌫", Action::Back, "Back: out of a page, to the sidebar", [(Esc, N), (Char('h'), N), (Backspace, N)]),
    b!(Global, "R", Action::Refresh, "Ask the server again", [(Char('R'), S), (Char('R'), N)]),
    b!(Global, "m", Action::Mouse, "Mouse on or off (off: the terminal selects text)", [(Char('m'), N)]),
    b!(Global, "I", Action::Images, "Covers on or off", [(Char('I'), S), (Char('I'), N)]),
    b!(Global, "?", Action::Help, "This help", [(Char('?'), N), (Char('?'), S), (F(1), N)]),
    b!(Global, "q / ctrl c", Action::Quit, "Quit", [(Char('q'), N), (Char('c'), C)]),
    b!(List, "↑ ↓ / k j", Action::Up, "Move", [(Up, N), (Char('k'), N)]),
    b!(List, "", Action::Down, "", [(Down, N), (Char('j'), N)]),
    b!(List, "g / G", Action::Top, "First or last", [(Home, N), (Char('g'), N)]),
    b!(List, "", Action::Bottom, "", [(End, N), (Char('G'), S), (Char('G'), N)]),
    b!(List, "pgup pgdn / ctrl u d", Action::PageUp, "A page up or down", [(PageUp, N), (Char('u'), C)]),
    b!(List, "", Action::PageDown, "", [(PageDown, N), (Char('d'), C)]),
    b!(List, "enter / l", Action::Open, "Open, or play from here", [(Enter, N), (Char('l'), N)]),
    b!(List, "a / A", Action::Enqueue, "Add to the queue, or play next", [(Char('a'), N)]),
    b!(List, "", Action::PlayNext, "", [(Char('A'), S), (Char('A'), N)]),
    b!(List, "x / X", Action::PlayAll, "Play the whole page, or shuffle it", [(Char('x'), N)]),
    b!(List, "", Action::ShuffleAll, "", [(Char('X'), S), (Char('X'), N)]),
    b!(List, "D", Action::Download, "Download", [(Char('D'), S), (Char('D'), N)]),
    b!(List, "f", Action::Star, "Favorite or not", [(Char('f'), N)]),
    b!(Grid, "← →", Action::Left, "Move along the row", [(Left, N)]),
    b!(Grid, "", Action::Right, "", [(Right, N)]),
    b!(Edit, "d / delete", Action::Remove, "Take it out (of the queue, or off this computer)", [(Char('d'), N), (Delete, N)]),
    b!(Edit, "u", Action::Undo, "Put back the song just taken out of the queue", [(Char('u'), N)]),
    b!(Edit, "K / J", Action::MoveUp, "Move it up or down", [(Char('K'), S), (Char('K'), N)]),
    b!(Edit, "", Action::MoveDown, "", [(Char('J'), S), (Char('J'), N)]),
    b!(Lyrics, "[ ]", Action::Later, "Words later or sooner, for lyrics timed wrong", [(Char('['), N)]),
    b!(Lyrics, "", Action::Sooner, "", [(Char(']'), N)]),
    b!(Lyrics, "0", Action::Unnudge, "Back to their own timing", [(Char('0'), N)]),
    b!(Lyrics, "enter", Action::Open, "Play from that line", [(Enter, N)]),
    b!(Eq, "← → / h l", Action::Left, "Choose a control or a band (seek with , and . here)", [(Left, N), (Char('h'), N)]),
    b!(Eq, "", Action::Right, "", [(Right, N), (Char('l'), N)]),
    b!(Eq, "↑ ↓ / k j", Action::Increase, "Change it: a band by ½ dB, a switch on or off, the next choice", [(Up, N), (Char('k'), N)]),
    b!(Eq, "", Action::Decrease, "", [(Down, N), (Char('j'), N)]),
    b!(Eq, "enter", Action::Open, "Switch, choose presets, add a band, back to flat", [(Enter, N)]),
    b!(Eq, "d / delete", Action::Remove, "Remove the band (parametric)", [(Char('d'), N), (Delete, N)]),
    b!(Values, "[ ]", Action::GroupBack, "The group before or after", [(Char('['), N)]),
    b!(Values, "", Action::GroupOn, "", [(Char(']'), N)]),
    b!(Values, "← → / h l", Action::Decrease, "Change the value (seek with , and . here)", [(Left, N), (Char('h'), N)]),
    b!(Values, "", Action::Increase, "", [(Right, N), (Char('l'), N)]),
    b!(Values, "enter", Action::Open, "Switch, choose or open", [(Enter, N)]),
];

/// The action `key` has in the first of `scopes` that binds it.
pub fn action(key: &KeyEvent, scopes: &[Scope]) -> Option<Action> {
    // Terminals differ on reporting shift with shifted characters, so both forms are bound.
    let mods = key.modifiers & (KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT);
    for scope in scopes {
        for b in BINDINGS.iter().filter(|b| b.scope == *scope) {
            if b.keys.iter().any(|(code, m)| *code == key.code && *m == mods) {
                return Some(b.action);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn focused_scope_wins() {
        let right = key(Right, N);
        assert_eq!(action(&right, &[Scope::Global]), Some(Action::SeekForward));
        assert_eq!(action(&right, &[Scope::Values, Scope::List, Scope::Global]), Some(Action::Increase));
        assert_eq!(action(&key(Char('h'), N), &[Scope::List, Scope::Global]), Some(Action::Back));
        assert_eq!(action(&key(Char('d'), N), &[Scope::Edit, Scope::List, Scope::Global]), Some(Action::Remove));
        assert_eq!(action(&key(Left, N), &[Scope::Grid, Scope::List, Scope::Global]), Some(Action::Left));
        assert_eq!(action(&key(Char('G'), S), &[Scope::List, Scope::Global]), Some(Action::Bottom));
    }

    #[test]
    fn each_scope_starts_labelled() {
        // An unlabelled binding continues the row above, so the first of each scope needs a label.
        for scope in Scope::ALL {
            let first = BINDINGS.iter().find(|b| b.scope == scope).expect("scope has bindings");
            assert!(!first.label.is_empty(), "{scope:?}");
        }
    }

    #[test]
    fn no_duplicate_keys_per_scope() {
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                if a.scope == b.scope {
                    for k in a.keys {
                        assert!(!b.keys.contains(k), "{k:?} twice in {:?}", a.scope);
                    }
                }
            }
        }
    }
}
