use super::*;

fn repo_metadata() -> cargo_metadata::Metadata {
    workspace_metadata().unwrap()
}

fn default_runtime() -> ax_config::RuntimeValues {
    ax_config::resolve_runtime(None, Default::default(), &Default::default()).unwrap()
}

fn repo_axbuild_dir() -> ::std::path::PathBuf {
    repo_metadata()
        .target_directory
        .into_std_path_buf()
        .join("axbuild")
}

mod config_file;
mod info;
mod metadata;
mod platform;
mod std_features;
mod std_metadata;
