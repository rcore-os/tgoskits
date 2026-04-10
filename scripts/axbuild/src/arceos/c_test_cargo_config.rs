use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use cargo_metadata::MetadataCommand;

const TEMPLATE_REL_PATH: &str = "scripts/arceos-c-test-cargo-config.template.toml";
pub(super) const MANAGED_MARKER: &str = "# axbuild-managed: arceos-c-test-cargo-config";
const PATCH_BLOCK_MARKER: &str = "# axbuild-managed-patches: appended below";

pub(super) fn prepare_c_test_cargo_config(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let arceos_dir = workspace_root.join("os/arceos");
    let config_path = arceos_dir.join(".cargo/config.toml");

    ensure_managed_destination(&config_path)?;

    let template_path = workspace_root.join(TEMPLATE_REL_PATH);
    let template = fs::read_to_string(&template_path)
        .with_context(|| format!("failed to read {}", template_path.display()))?;
    let patches = discover_patch_paths(workspace_root, &arceos_dir)?;
    let rendered = render_c_test_cargo_config(&template, &patches)?;

    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(&config_path, rendered)
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    Ok(config_path)
}

fn ensure_managed_destination(config_path: &Path) -> anyhow::Result<()> {
    if !config_path.exists() {
        return Ok(());
    }

    let existing = fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    if is_managed_c_test_config(&existing) {
        Ok(())
    } else {
        bail!(
            "refusing to overwrite user-managed cargo config at {}; remove it or add the `{}` \
             marker if this file should be tool-managed",
            config_path.display(),
            MANAGED_MARKER
        )
    }
}

fn is_managed_c_test_config(contents: &str) -> bool {
    contents.contains(MANAGED_MARKER)
}

fn render_c_test_cargo_config(
    template: &str,
    patches: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<String> {
    validate_template(template)?;

    let mut rendered = template.to_string();
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str("[patch.crates-io]\n");
    for (crate_name, relative_path) in patches {
        let path_literal = toml::Value::String(display_path(relative_path)).to_string();
        rendered.push_str(&format!("{crate_name} = {{ path = {path_literal} }}\n"));
    }
    Ok(rendered)
}

fn validate_template(template: &str) -> anyhow::Result<()> {
    if !template.contains(MANAGED_MARKER) {
        bail!("ArceOS C-test cargo config template is missing `{MANAGED_MARKER}`")
    }
    if !template.contains(PATCH_BLOCK_MARKER) {
        bail!("ArceOS C-test cargo config template is missing `{PATCH_BLOCK_MARKER}`")
    }
    Ok(())
}

fn discover_patch_paths(
    workspace_root: &Path,
    arceos_dir: &Path,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let root_patch_paths = root_patch_paths(workspace_root)?;
    let repo_local_packages = discover_repo_local_packages(workspace_root)?;
    let registry_dependency_names =
        registry_dependency_names_in_workspace(&arceos_dir.join("Cargo.toml"))?;
    let mut patches = BTreeMap::new();

    for (crate_name, package_dir) in &root_patch_paths {
        let relative_path = relative_path_from(arceos_dir, package_dir)?;
        patches.insert(crate_name.clone(), relative_path);
    }

    for crate_name in registry_dependency_names {
        if patches.contains_key(&crate_name) {
            continue;
        }

        let package_dir = match repo_local_packages.get(&crate_name).map(Vec::as_slice) {
            Some([only_path]) => only_path.clone(),
            Some(paths) => {
                let rendered = paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "multiple repo-local packages named `{crate_name}` found while generating \
                     ArceOS C-test patches: {rendered}"
                )
            }
            None => continue,
        };

        let relative_path = relative_path_from(arceos_dir, &package_dir)?;
        patches.insert(crate_name, relative_path);
    }

    Ok(patches)
}

fn root_patch_paths(workspace_root: &Path) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let root_manifest = workspace_root.join("Cargo.toml");
    let contents = fs::read_to_string(&root_manifest)
        .with_context(|| format!("failed to read {}", root_manifest.display()))?;
    let value: toml::Value = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", root_manifest.display()))?;
    let mut patches = BTreeMap::new();

    let Some(table) = value
        .get("patch")
        .and_then(|value| value.get("crates-io"))
        .and_then(toml::Value::as_table)
    else {
        return Ok(patches);
    };

    for (crate_name, entry) in table {
        let Some(path) = entry.get("path").and_then(toml::Value::as_str) else {
            continue;
        };
        patches.insert(
            crate_name.clone(),
            normalize_path(&workspace_root.join(path)),
        );
    }

    Ok(patches)
}

fn discover_repo_local_packages(
    workspace_root: &Path,
) -> anyhow::Result<BTreeMap<String, Vec<PathBuf>>> {
    let mut packages = BTreeMap::<String, Vec<PathBuf>>::new();
    let mut stack = vec![workspace_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        for entry in
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry under {}", dir.display()))?;
            let path = entry.path();

            if path.is_dir() {
                let Some(name) = path.file_name().and_then(OsStr::to_str) else {
                    continue;
                };
                if matches!(name, "target" | ".git") {
                    continue;
                }
                stack.push(path);
                continue;
            }

            if path.file_name() != Some(OsStr::new("Cargo.toml")) {
                continue;
            }

            let Some(package_name) = manifest_package_name(&path)? else {
                continue;
            };
            let package_dir = path.parent().map(normalize_path).ok_or_else(|| {
                anyhow::anyhow!("manifest path has no parent: {}", path.display())
            })?;
            packages.entry(package_name).or_default().push(package_dir);
        }
    }

    for paths in packages.values_mut() {
        paths.sort();
        paths.dedup();
    }

    Ok(packages)
}

fn manifest_package_name(manifest_path: &Path) -> anyhow::Result<Option<String>> {
    let contents = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let value: toml::Value = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    Ok(value
        .get("package")
        .and_then(toml::Value::as_table)
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string))
}

fn registry_dependency_names_in_workspace(
    arceos_manifest: &Path,
) -> anyhow::Result<BTreeSet<String>> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .manifest_path(arceos_manifest)
        .exec()
        .with_context(|| {
            format!(
                "failed to get cargo metadata for {}",
                arceos_manifest.display()
            )
        })?;

    Ok(metadata
        .packages
        .into_iter()
        .flat_map(|package| package.dependencies.into_iter())
        .filter_map(|dependency| {
            dependency
                .source
                .filter(|source| source.repr.contains("crates.io-index"))
                .map(|_| dependency.name)
        })
        .collect())
}

fn relative_path_from(base: &Path, target: &Path) -> anyhow::Result<PathBuf> {
    let base = normalize_path(base);
    let target = normalize_path(target);
    if base.is_absolute() != target.is_absolute() {
        bail!(
            "cannot compute relative path between {} and {}",
            base.display(),
            target.display()
        );
    }

    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let common_prefix_len = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(lhs, rhs)| lhs == rhs)
        .count();

    let mut relative = PathBuf::new();
    for component in &base_components[common_prefix_len..] {
        if matches!(component, Component::Normal(_)) {
            relative.push("..");
        }
    }
    for component in &target_components[common_prefix_len..] {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        relative.push(".");
    }
    Ok(relative)
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn render_c_test_cargo_config_preserves_template_and_sorts_patches() {
        let template =
            format!("{MANAGED_MARKER}\n{PATCH_BLOCK_MARKER}\n[net]\ngit-fetch-with-cli = true\n");
        let mut patches = BTreeMap::new();
        patches.insert(
            "z-last".to_string(),
            PathBuf::from("../../components/z-last"),
        );
        patches.insert("a-first".to_string(), PathBuf::from("api/a-first"));

        let rendered = render_c_test_cargo_config(&template, &patches).unwrap();

        assert!(rendered.starts_with(&template));
        assert!(rendered.contains("[patch.crates-io]\n"));
        let a_pos = rendered.find("a-first =").unwrap();
        let z_pos = rendered.find("z-last =").unwrap();
        assert!(a_pos < z_pos);
    }

    #[test]
    fn discover_patch_paths_prefers_root_patch_for_duplicate_local_package_names() {
        let root = repo_root();
        let patches = discover_patch_paths(&root, &root.join("os/arceos")).unwrap();

        assert_eq!(
            patches.get("ax-plat-riscv64-qemu-virt"),
            Some(&PathBuf::from(
                "../../components/axplat_crates/platforms/axplat-riscv64-qemu-virt"
            ))
        );
    }

    #[test]
    fn discover_patch_paths_covers_repo_local_arceos_dependencies_missing_from_root_patch() {
        let root = repo_root();
        let patches = discover_patch_paths(&root, &root.join("os/arceos")).unwrap();

        for crate_name in [
            "ax-crate-interface",
            "ax-allocator",
            "axbacktrace",
            "ax-lazyinit",
            "ax-sched",
            "ax-cap-access",
            "ax-cpumask",
            "rsext4",
        ] {
            assert!(
                patches.contains_key(crate_name),
                "expected generated C-test patch for `{crate_name}`"
            );
        }
    }

    #[test]
    fn discover_patch_paths_keeps_root_patch_entries_for_transitive_local_crates() {
        let root = repo_root();
        let patches = discover_patch_paths(&root, &root.join("os/arceos")).unwrap();

        assert_eq!(
            patches.get("ax-page-table-entry"),
            Some(&PathBuf::from(
                "../../components/page_table_multiarch/page_table_entry"
            ))
        );
    }

    #[test]
    fn relative_path_generation_uses_os_arceos_workspace_root() {
        let relative = relative_path_from(
            Path::new("/repo/os/arceos"),
            Path::new("/repo/components/crate_interface"),
        )
        .unwrap();

        assert_eq!(relative, PathBuf::from("../../components/crate_interface"));
    }

    #[test]
    fn ensure_managed_destination_allows_rewriting_managed_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, format!("{MANAGED_MARKER}\nmanaged = true\n")).unwrap();

        ensure_managed_destination(&config_path).unwrap();
    }

    #[test]
    fn ensure_managed_destination_rejects_foreign_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "[net]\ngit-fetch-with-cli = true\n").unwrap();

        let err = ensure_managed_destination(&config_path)
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to overwrite user-managed cargo config"));
    }
}
