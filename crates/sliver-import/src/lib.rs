//! Importers for the configs sliver replaces or borrows from. Each turns a foreign config into a
//! sliver `config.toml` (and stylesheet), validated with `sliver_config`, and reports what didn't
//! carry over instead of dropping it silently.

pub mod tinydfr;
pub mod waybar;
