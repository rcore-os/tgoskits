use ::std::fs;
use tempfile::tempdir;

use super::*;

fn repo_metadata() -> cargo_metadata::Metadata {
    workspace_metadata().unwrap()
}

fn repo_axbuild_dir() -> ::std::path::PathBuf {
    repo_metadata()
        .target_directory
        .into_std_path_buf()
        .join("axbuild")
}

mod info;
mod metadata;
mod platform;
mod std_features;
mod std_linker;
mod std_metadata;
mod target_specs;
