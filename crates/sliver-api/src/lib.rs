//! The public session D-Bus API (`docs/api.md` §2–5). Owner: cedar (ADR-0003).
//!
//! - [`item`]: the published-item vocabulary, parsed leniently into typed items.
//! - [`registry`]: the sans-IO model: clients, contexts, ownership, limits, sandbox tiers,
//!   who fills App controls / Status items, optimistic state and events.
//! - [`identity`]: verified client identity from `/proc` (pid, Flatpak instance, app unit).

pub mod identity;
pub mod item;
pub mod registry;
