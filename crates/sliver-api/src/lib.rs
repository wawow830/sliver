//! The public session D-Bus API, served by the agent at `io.github.wawow830.Sliver` (names and
//! limits in `sliver_ipc::dbus`, interfaces in `docs/dbus/io.github.wawow830.Sliver.xml`, which a
//! test compares with what's served). Apps never draw: they publish declarative items in a
//! *context*, the agent draws them and sends `Event`s to the owner alone.
//!
//! - A context's scope: `app` fills the user's App controls placeholder while the caller's own
//!   window has focus (same pid, Flatpak instance or app unit; window-bound first, then
//!   `include-children`, then `target` contexts matching a selector); `status` fills Status
//!   items. A `modal` app context covers all but the first zone, with a ✕. Locked: nothing shows.
//! - Every published item is a module `app/<provider>#<item>`: placed, styled, bound and
//!   formatted like any (sliver-config(5)); the placeholders hold what isn't placed elsewhere.
//! - Contexts live as long as the owner's connection; republish when the name gets a new owner.
//!   Sandboxed clients get `app` scope, and `status` once the user allows them; `Control1` and
//!   `Desktop1` are for unsandboxed clients. Updates are coalesced to 30 redraws a second.
//! - Breaking changes make `TouchBar2`; additions bump `Version` (`API_MINOR`).
//!
//! - [`item`]: the published-item vocabulary, parsed leniently into typed items.
//! - [`registry`]: the sans-IO model: clients, contexts, ownership, limits, sandbox tiers,
//!   who fills App controls / Status items, optimistic state and events.
//! - [`identity`]: verified client identity from `/proc` (pid, Flatpak instance, app unit).
//! - [`service`]: the zbus layer (`TouchBar1`, `Context1`, `Control1`, `Desktop1`) and [`service::Api`],
//!   the handle the agent core uses.

pub mod identity;
pub mod item;
pub mod registry;
pub mod service;
