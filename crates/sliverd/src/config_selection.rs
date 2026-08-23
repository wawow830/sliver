use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigSelection {
    Path(PathBuf),
    Default,
}
