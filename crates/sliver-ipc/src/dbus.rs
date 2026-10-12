//! Names and limits of the public session D-Bus API (`docs/api.md` §2–5).
//!
//! The namespace `io.github.wawow830.Sliver` was chosen by the human (H1). Change it here and only here.

/// Bus name owned by `sliver-agent` on the session bus.
pub const BUS_NAME: &str = "io.github.wawow830.Sliver";
/// Object path of the root object (`TouchBar1`, `Control1`, `Desktop1`).
pub const ROOT_PATH: &str = "/io/github/wawow830/Sliver";
/// Contexts live at `CONTEXT_PATH_PREFIX/<n>`.
pub const CONTEXT_PATH_PREFIX: &str = "/io/github/wawow830/Sliver/context";

pub const IFACE_TOUCHBAR: &str = "io.github.wawow830.Sliver.TouchBar1";
pub const IFACE_CONTEXT: &str = "io.github.wawow830.Sliver.Context1";
pub const IFACE_CONTROL: &str = "io.github.wawow830.Sliver.Control1";
pub const IFACE_DESKTOP: &str = "io.github.wawow830.Sliver.Desktop1";

/// Minor version reported in the `Version` property. Bump it for additive changes.
pub const API_MINOR: u32 = 0;

/// D-Bus error names.
pub mod error {
    pub const ACCESS_DENIED: &str = "io.github.wawow830.Sliver.Error.AccessDenied";
    pub const LIMIT_EXCEEDED: &str = "io.github.wawow830.Sliver.Error.LimitExceeded";
    pub const INVALID_ARGS: &str = "io.github.wawow830.Sliver.Error.InvalidArgs";
    pub const BUSY: &str = "io.github.wawow830.Sliver.Error.Busy";
}

/// Per-connection abuse limits (`docs/api.md` §4).
pub mod limits {
    pub const CONTEXTS_PER_CLIENT: usize = 16;
    pub const ITEMS_PER_CONTEXT: usize = 128;
    pub const ICON_BYTES: usize = 256 * 1024;
    pub const ICON_BYTES_PER_CONTEXT: usize = 512 * 1024;
    pub const LABEL_CHARS: usize = 64;
    /// Segments of a `segmented` item.
    pub const SEGMENTS: usize = 32;
    /// Candidates of a `scrubber` item.
    pub const CANDIDATES: usize = 256;
    /// A scrubber shows at most this many candidates at once.
    pub const SCRUBBER_VISIBLE: u32 = 24;
    /// Sustained calls per second before a client is throttled.
    pub const CALLS_PER_SEC: u32 = 120;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_share_one_namespace() {
        for iface in [IFACE_TOUCHBAR, IFACE_CONTEXT, IFACE_CONTROL, IFACE_DESKTOP] {
            assert!(iface.starts_with(BUS_NAME));
        }
        assert_eq!(ROOT_PATH, format!("/{}", BUS_NAME.replace('.', "/")));
        assert!(CONTEXT_PATH_PREFIX.starts_with(ROOT_PATH));
    }
}
