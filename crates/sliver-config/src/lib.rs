//! Sliver's config language: the waybar model translated to a touch strip.
//!
//! The spec is `docs/customization.md`, and ADR-0004 records the decisions. This crate parses
//! and validates config files and resolves what each layer shows for a given context. It
//! imports tiny-dfr configs. It does no rendering and no pixel layout (that's sliver-render), and
//! it runs no modules (that's the agent's module runtime).
//!
//! ```
//! use sliver_config::{Context, Hardware, load_str};
//! let loaded = load_str(r#"
//!     [[rules]]
//!     when = { app-id = ["foot", "kitty"] }
//!     layout = "fkeys"
//! "#, std::path::Path::new("."));
//! assert!(loaded.diagnostics.ok());
//! let ctx = Context::new().with("app_id", "foot").with("physical_esc", true);
//! let bar = loaded.config.resolve("base", &ctx).unwrap();
//! assert_eq!(bar.layout, "fkeys");
//! assert!(loaded.config.validate(Hardware { physical_esc: true }).ok());
//! ```

mod action;
mod condition;
mod diag;
pub mod format;
mod keys;
mod keys_table;
mod load;
mod model;
mod module;
pub mod preset;
pub mod profile;
mod resolve;
pub mod tinydfr;
mod validate;
mod values;
pub mod writer;

pub use action::{Action, DBusCall, LayerMode, MAX_MACRO_STEPS, MAX_MACRO_TIME};
pub use condition::{CONTEXT_KEYS, Condition, Context, CtxValue, Matcher, canonical_key};
pub use diag::{Diagnostics, Issue};
pub use format::{FmtValue, Formatted, IconRef, State, Template, ValueMap, Values, format_module};
pub use keys::Key;
pub use load::{Loaded, edit_path, find_config, load, load_file, load_str, search_path};
pub use model::{
    ApiPrefs, AppPolicy, AppPrefs, BAR_GESTURES, BUILTIN, Bar, Config, Display, DisplayOverride,
    FnMode, LayerOptions, Layout, ModuleRef, ProviderPrefs, Rule, SCHEMA_VERSION, Zone,
};
pub use module::{COMMON_KEYS, Common, Gesture, MODULE_TYPES, MenuEntry, ModuleId};
pub use resolve::{
    HIDDEN_WHILE_LOCKED, MAX_DEPTH, ModuleInstance, ResolveError, Resolved, ResolvedZone,
    deep_merge,
};
pub use validate::{COMFORTABLE_TARGET_PX, Hardware, TINY_TARGET_PX};
pub use values::{Align, FireOn, Interval, MIN_INTERVAL, Width, parse_duration};
pub use writer::{Editor, parse_cli_value};

/// Native Touch Bar panel size in landscape orientation (width, height) on Apple Silicon.
pub const PANEL_SIZE: (u32, u32) = (2008, 60);
