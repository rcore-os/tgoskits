mod manifest;
mod refs;
mod selection;

#[cfg(test)]
mod tests;

pub(crate) use refs::changed_paths_since;
pub(crate) use selection::{IncrementalPackageSelection, top_level_affected_workspace_packages};

pub(crate) fn select_incremental_packages(
    workspace_root: &std::path::Path,
    metadata: &cargo_metadata::Metadata,
    workspace_packages: &[cargo_metadata::Package],
    since: &str,
) -> anyhow::Result<IncrementalPackageSelection> {
    match refs::changed_paths_since_with_base(workspace_root, since) {
        Ok((paths, diff_base)) => {
            let root_manifest_change = if paths
                .iter()
                .any(|path| path == std::path::Path::new(manifest::ROOT_MANIFEST))
            {
                Some(manifest::root_manifest_change_since(
                    workspace_root,
                    &diff_base,
                )?)
            } else {
                None
            };

            selection::select_incremental_packages_for_paths_with_root_manifest_change(
                workspace_root,
                metadata,
                workspace_packages,
                paths,
                root_manifest_change,
            )
        }
        Err(err) => Ok(IncrementalPackageSelection::Full {
            reason: format!("failed to diff against `{since}`: {err:#}"),
        }),
    }
}
