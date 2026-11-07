//! What a gesture on a cell does, decided when the cell is built.
//!
//! A module's behaviour is a set of named verbs (`set`, `toggle-mute`, `next`), named by its
//! default bindings, `actions = { on-hold = "…" }` and `{ module-action = "…" }`. Config
//! [`Action`]s and verbs resolve to a [`Do`]: a config action, or an [`Act`] carrying what it needs
//! (which player, which window, which level).

use std::time::Duration;

use sliver_config::Action;

use crate::agent::SessionOp;

#[derive(Debug, Clone, PartialEq)]
pub enum Do {
    Run(Action),
    /// Something only the agent can do.
    Agent(Act),
    /// A macro: these in order, `Run(Sleep)` waiting between them.
    Steps(Vec<Do>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Session(SessionOp),
    /// Ask the compositor (focus a workspace or window, close one, switch keyboard layout).
    Desktop(sliver_desktop::control::Action),
    /// A drag sets this level to its value (0–100).
    Set(Level),
    /// A drag seeks this player (`None`: the session's) to its value (0–100 of the track).
    Seek {
        player: Option<String>,
    },
    /// The agent-side state of the cell `key` changes.
    Ui {
        key: String,
        change: UiChange,
    },
    /// Open these entries as a sub-bar, from the cell `key` (its ✕ takes that cell's place).
    Menu {
        key: String,
        items: Vec<MenuItem>,
    },
    /// A menu entry: close the menu, then do this.
    Choose(Box<Do>),
    /// Close the menu or drawer covering the bar.
    Close,
    /// Bound by a name the module doesn't have: logged when it fires.
    Unknown {
        kind: String,
        name: String,
    },
}

/// A level a slider sets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Screen,
    Keyboard,
    Volume { mic: bool },
}

impl Level {
    pub fn set(self, pct: u8) -> SessionOp {
        match self {
            Level::Screen => SessionOp::ScreenBrightness(pct),
            Level::Keyboard => SessionOp::KeyboardBrightness(pct),
            Level::Volume { mic } => SessionOp::Volume { mic, pct },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiChange {
    /// Open or close (a drawer, a collapsed slider).
    Expand,
    /// The clock shows its next (+1) or previous (−1) time zone.
    Zone(i32),
    /// The mpris module shows this player.
    Player(String),
    /// Start, pause or restart the timer, which runs `total` from the start.
    Timer {
        total: Duration,
    },
    TimerReset,
    Emoji(EmojiOp),
}

/// The emoji strip's own controls.
#[derive(Debug, Clone, PartialEq)]
pub enum EmojiOp {
    /// Type this emoji.
    Pick {
        insert: Insert,
        text: String,
    },
    /// Show this emoji's skin tones in place of the page, or stop.
    Tones(Option<String>),
    Search,
    /// Add a letter or space to the query.
    Type(char),
    /// Erase the last letter, or the whole query.
    Erase {
        all: bool,
    },
    /// Back to the groups.
    EndSearch,
    Tab(usize),
    Page(i64),
}

/// How a picked emoji gets into the focused app (`insert`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Insert {
    /// `wtype` when installed, else copy.
    Auto,
    /// Type it through a virtual keyboard.
    Wtype,
    /// Copy it and press Ctrl+V through sliverd (works anywhere, replaces the clipboard).
    Paste,
    /// Only copy it.
    Copy,
}

impl Insert {
    pub fn parse(s: &str) -> Option<Insert> {
        Some(match s {
            "auto" => Insert::Auto,
            "wtype" => Insert::Wtype,
            "paste" => Insert::Paste,
            "copy" => Insert::Copy,
            _ => return None,
        })
    }
}

/// One entry of a sub-bar.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub label: String,
    pub icon: Option<String>,
    pub on_tap: Do,
}

/// A module's verbs: what its names mean on this cell.
pub type Verbs = Vec<(&'static str, Do)>;

impl Do {
    /// A config action, with module-action names looked up in the verbs of the module `kind`.
    pub fn bind(a: &Action, kind: &str, verbs: &[(&'static str, Do)]) -> Do {
        match a {
            Action::ModuleAction(name) => verbs
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, d)| d.clone())
                .unwrap_or_else(|| {
                    Do::Agent(Act::Unknown {
                        kind: kind.to_owned(),
                        name: name.clone(),
                    })
                }),
            Action::Macro(steps) => {
                Do::Steps(steps.iter().map(|s| Do::bind(s, kind, verbs)).collect())
            }
            a => Do::Run(a.clone()),
        }
    }

    pub fn action(&self) -> Option<&Action> {
        match self {
            Do::Run(a) => Some(a),
            _ => None,
        }
    }
}

impl From<Act> for Do {
    fn from(a: Act) -> Do {
        Do::Agent(a)
    }
}
