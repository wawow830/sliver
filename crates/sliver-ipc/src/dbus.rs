//! Names of the agent's session D-Bus interface (`Control1`).

/// Bus name owned by `sliver-agent` on the session bus.
pub const BUS_NAME: &str = "io.github.wawow830.Sliver";
/// Object path of `Control1`.
pub const ROOT_PATH: &str = "/io/github/wawow830/Sliver";
pub const IFACE_CONTROL: &str = "io.github.wawow830.Sliver.Control1";

/// D-Bus error names.
pub mod error {
    pub const ACCESS_DENIED: &str = "io.github.wawow830.Sliver.Error.AccessDenied";
    pub const INVALID_ARGS: &str = "io.github.wawow830.Sliver.Error.InvalidArgs";
    pub const FAILED: &str = "io.github.wawow830.Sliver.Error.Failed";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_share_one_namespace() {
        assert!(IFACE_CONTROL.starts_with(BUS_NAME));
        for e in [error::ACCESS_DENIED, error::INVALID_ARGS, error::FAILED] {
            assert!(e.starts_with(BUS_NAME));
        }
        assert_eq!(ROOT_PATH, format!("/{}", BUS_NAME.replace('.', "/")));
    }
}
