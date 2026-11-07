//! Sliver's config language (`sliver-config(5)`): parses and validates config files and resolves
//! what each layer shows for a given context. Pixel layout is sliver-render's, modules run in the
//! agent, and foreign configs are converted by `sliver-import`.
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
mod files;
pub mod format;
mod keys;
mod keys_table;
mod load;
mod model;
mod module;
mod options;
pub mod profile;
mod resolve;
mod rewrite;
mod validate;
mod values;
pub mod writer;

pub use action::{Action, DBusCall, LayerMode, MAX_MACRO_STEPS, MAX_MACRO_TIME};
pub use condition::{CONTEXT_KEYS, Condition, Context, CtxValue, Matcher};
pub use diag::{Diagnostics, Issue};
pub use format::{
    FmtValue, Format, Formatted, IconRef, State, Template, ValueMap, Values, format_module,
};
pub use keys::Key;
pub use load::{
    Loaded, edit_path, find_config, load, load_file, load_str, load_str_as, search_path,
};
pub use model::{
    ApiPrefs, AppPolicy, AppPrefs, BAR_GESTURES, BUILTIN, Bar, Config, Display, DisplayOverride,
    FnMode, LayerOptions, Layout, ModuleRef, ProviderPrefs, Rule, SCHEMA_VERSION, Zone,
};
pub use module::{COMMON_KEYS, Common, Gesture, MenuEntry, ModuleId};
pub use options::{
    Drawer, Keys, Kind, MAX_SIGNAL, MODULE_TYPES, Opt, key_kind, own_keys, takes_key,
};
pub use resolve::{
    HIDDEN_WHILE_LOCKED, MAX_DEPTH, ModuleInstance, ResolveError, Resolved, ResolvedZone,
    deep_merge,
};
pub use rewrite::Rewrite;
pub use validate::{COMFORTABLE_TARGET_PX, Hardware, TINY_TARGET_PX};
pub use values::{Align, FireOn, Interval, MIN_INTERVAL, Width, parse_duration};
pub use writer::{Editor, parse_cli_value};

/// Native Touch Bar panel size in landscape orientation (width, height) on Apple Silicon.
pub const PANEL_SIZE: (u32, u32) = (2008, 60);
