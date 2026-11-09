use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArceosBuildMode {
    Rust,
    AppC { app_dir: PathBuf, app_name: String },
}
