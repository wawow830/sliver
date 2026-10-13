//! Lock-screen privacy (ux.md §5): while the screen is locked, nothing private reaches the bar,
//! whatever the layout says. Two layers, so a new module or a clever format string can't leak:
//!
//! 1. [`sanitize`]: modules are built from a copy of the world with the private data removed:
//!    media title/artist/album/art, the latest notification's text, window titles and the
//!    window list, script output, everything apps published.
//! 2. [`allowed`]: cells of kinds that exist to show private things are dropped: `custom/*`
//!    (arbitrary script output, and running commands from a locked bar), app-published items and
//!    their App controls / Status items slots, `window`, `taskbar`, and the emoji picker (its
//!    recents are a history).
//!
//! What stays: keys (Esc, F-keys, media keys), sliders, toggles, clock, battery, workspaces,
//! keyboard layout, the notification *count*, media play/pause without the title.

use crate::modules::{Cell, World};

/// Module kinds never shown while locked.
const HIDDEN_KINDS: &[&str] = &[
    "custom",
    // A picture a script picked (album art, a photo).
    "image",
    // A history of something (custom-graph/<name> is kind `graph` too).
    "graph",
    "app-controls",
    "status-items",
    "slot",
    "app",
    "status",
    "window",
    "taskbar",
    "emoji",
];

/// The world as a locked screen may see it.
pub fn sanitize(world: &World) -> World {
    let mut w = world.clone();
    let s = &mut w.session;
    for m in s.media.iter_mut().chain(s.players.iter_mut()) {
        m.title.clear();
        m.artist.clear();
        m.album.clear();
        m.art_url = None;
    }
    s.notifications.latest = None;
    w.media_art = None;
    w.desktop.window = None;
    w.desktop.windows.clear();
    w.outputs.clear();
    w.published = Default::default();
    w
}

/// May this cell be drawn on a locked screen?
pub fn allowed(c: &Cell) -> bool {
    c.published.is_none() && !HIDDEN_KINDS.contains(&c.kind.as_str())
}
