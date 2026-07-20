use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArceosCArtifactPaths {
    pub(crate) target_dir: PathBuf,
    pub(crate) out_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct ArceosCBuildInput {
    pub(crate) app_dir: PathBuf,
    pub(crate) app_name: String,
    pub(crate) target_dir: PathBuf,
    pub(crate) out_dir: PathBuf,
    pub(crate) features: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ArceosCBuildOutput {
    pub(crate) elf_path: PathBuf,
}

pub(crate) fn default_c_app_artifact_paths(
    workspace_root: &Path,
    app_name: &str,
) -> ArceosCArtifactPaths {
    let target_dir = crate::context::axbuild_tmp_dir(workspace_root)
        .join("arceos-c")
        .join("cargo");
    let out_dir = crate::context::axbuild_tmp_dir(workspace_root)
        .join("arceos-c")
        .join("apps")
        .join(sanitize_name(app_name))
        .join("out");

    ArceosCArtifactPaths {
        target_dir,
        out_dir,
    }
}

pub(super) fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}
