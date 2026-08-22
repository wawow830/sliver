//! Legacy Sliver daemon and developer modes.
//!
//! Issue #16 removes this binary after the scriptable framework is complete.

fn main() -> anyhow::Result<()> {
    sliverd::legacy_main()
}
