//! Sliver's layout model and configuration schema.
//!
//! Config is TOML in `~/.config/sliver/` (user) and `/etc/sliver/` (system), with a versioned
//! schema and a tiny-dfr importer (ADR-0001). Stub: the model lands with T4/T8.

/// Current config schema version. Bump on breaking schema changes and add a migration.
pub const SCHEMA_VERSION: u32 = 1;

/// Native Touch Bar panel size in landscape orientation (width, height) on Apple Silicon.
pub const PANEL_SIZE: (u32, u32) = (2008, 60);

const _: () = assert!(SCHEMA_VERSION >= 1);
