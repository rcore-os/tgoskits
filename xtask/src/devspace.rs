use std::collections::{BTreeMap, HashMap, hash_map::Entry};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use serde::{Deserialize, Serialize};

const STATE_DIR: &str = ".devspace";
const STATE_FILE: &str = ".devspace/state.json";
const PATCH_BEGIN_MARKER: &str = "# >>> devspace patches >>>";
const PATCH_END_MARKER: &str = "# <<< devspace patches <<<";
const CRATES_IO_SOURCE_KEY: &str = "crates-io";

const DEVSPACE_REPOS: &[&str] = &[
    "arm_vcpu",
    "arm_vgic",
    "axaddrspace",
    "axdevice_base",
    "x86_vcpu",
    "x86_vlapic",
];

const DEVSPACE_REPO_OVERRIDES: &[(&str, &str)] = &[(
    "axdevice_base",
    "https://github.com/arceos-hypervisor/axdevice_base.git",
)];

pub fn start() -> Result<()> {
    let metadata = MetadataCommand::new()
        .exec()
        .context("Failed to run cargo metadata")?;
    let repos = resolve_dev_repos(&metadata)?;

    let mut state = load_state()?;
    ensure_submodules(&mut state, &repos)?;
    save_state(&state)?;

    let specs = compute_patch_specs(&metadata, &repos)?;
    apply_patches(&specs)?;

    state.patches = specs
        .into_iter()
        .map(|spec| PatchRecord {
            source: spec.source,
            crate_name: spec.crate_name,
        })
        .collect();
    save_state(&state)?;

    println!("devspace start completed");
    Ok(())
}

pub fn stop() -> Result<()> {
    let mut state = load_state()?;

    if !state.patches.is_empty() {
        remove_patches(&state.patches)?;
        state.patches.clear();
    }

    if !state.modules.is_empty() {
        remove_submodules(&state.modules)?;
        state.modules.clear();
    }

    save_state(&state)?;
    println!("devspace stop completed");
    Ok(())
}

fn ensure_submodules(state: &mut DevspaceState, repos: &[DevRepo]) -> Result<()> {
    for repo in repos {
        let dest_path = Path::new(&repo.dest);
        if dest_path.exists() {
            continue;
        }

        println!("Adding submodule {} -> {}", repo.git_url, repo.dest);
        run_git(&[
            "submodule",
            "add",
            "--force",
            repo.git_url.as_str(),
            repo.dest.as_str(),
        ])?;
        run_git(&[
            "submodule",
            "update",
            "--init",
            "--recursive",
            repo.dest.as_str(),
        ])?;
        match state.modules.entry(repo.name.clone()) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().path = repo.dest.clone();
            }
            Entry::Vacant(entry) => {
                entry.insert(ManagedModule {
                    name: repo.name.clone(),
                    path: repo.dest.clone(),
                });
            }
        }
    }

    Ok(())
}

fn remove_submodules(modules: &HashMap<String, ManagedModule>) -> Result<()> {
    for module in modules.values() {
        println!("Removing submodule {}", module.path);
        let path = module.path.as_str();
        let _ = run_git(&["submodule", "deinit", "-f", "--", path]);
        let git_modules_dir = Path::new(".git/modules").join(path);
        if git_modules_dir.exists() {
            fs::remove_dir_all(&git_modules_dir)
                .with_context(|| format!("Failed to remove {git_modules_dir:?}"))?;
        }
        if Path::new(path).exists() {
            let _ = run_git(&["rm", "-f", "--", path]);
            if Path::new(path).exists() {
                fs::remove_dir_all(path).with_context(|| format!("Failed to remove {path}"))?;
            }
        }
    }
    Ok(())
}

fn compute_patch_specs(metadata: &Metadata, repos: &[DevRepo]) -> Result<Vec<PatchSpec>> {
    let repo_map = build_repo_lookup(repos);
    let mut specs = BTreeMap::new();

    for pkg in &metadata.packages {
        if let Some(spec) = package_patch_spec(pkg, &repo_map) {
            specs
                .entry((spec.source.clone(), spec.crate_name.clone()))
                .or_insert(spec);
        }
    }

    for repo in repos {
        if !specs.contains_key(&(repo.source.clone(), repo.name.clone())) {
            return Err(anyhow!(
                "Failed to prepare patch for crate {} (source: {})",
                repo.name,
                repo.source
            ));
        }
    }

    Ok(specs.into_values().collect())
}

fn package_patch_spec(pkg: &Package, repo_map: &HashMap<String, &DevRepo>) -> Option<PatchSpec> {
    let source_raw = pkg.source.as_ref()?.to_string();
    let normalized = normalize_source(&source_raw)?;
    let key = repo_lookup_key(&normalized, pkg.name.as_str());
    let repo = repo_map.get(&key)?;
    let manifest = Path::new(pkg.manifest_path.as_str());
    let local_path = manifest_relative_dir(manifest)
        .map(|subdir| Path::new(&repo.dest).join(subdir))
        .unwrap_or_else(|| PathBuf::from(&repo.dest));
    Some(PatchSpec {
        source: repo.source.to_string(),
        crate_name: pkg.name.to_string(),
        path: to_unix_path(&local_path),
    })
}

fn apply_patches(specs: &[PatchSpec]) -> Result<()> {
    if specs.is_empty() {
        println!("No git dependencies matched managed repos; skipping patch stage");
        return Ok(());
    }

    let config_path = Path::new(".cargo/config.toml");
    let mut contents = if config_path.exists() {
        fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read {config_path:?}"))?
    } else {
        String::new()
    };

    let (cleaned, _) = strip_devspace_section(&contents);
    contents = cleaned;

    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    if !contents.is_empty() && !contents.ends_with("\n\n") {
        contents.push('\n');
    }

    contents.push_str(&render_devspace_section(specs));
    if !contents.ends_with('\n') {
        contents.push('\n');
    }

    fs::write(config_path, contents).with_context(|| format!("Failed to write {config_path:?}"))?;
    Ok(())
}

fn remove_patches(_: &[PatchRecord]) -> Result<()> {
    let config_path = Path::new(".cargo/config.toml");
    if !config_path.exists() {
        return Ok(());
    }

    let original = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {config_path:?}"))?;
    let (cleaned, removed) = strip_devspace_section(&original);

    if removed {
        fs::write(config_path, cleaned)
            .with_context(|| format!("Failed to write {config_path:?}"))?;
    }
    Ok(())
}

fn load_state() -> Result<DevspaceState> {
    let path = Path::new(STATE_FILE);
    if !path.exists() {
        return Ok(DevspaceState::default());
    }

    let contents = fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;
    let state =
        serde_json::from_str(&contents).with_context(|| format!("Failed to parse {path:?}"))?;
    Ok(state)
}

fn save_state(state: &DevspaceState) -> Result<()> {
    fs::create_dir_all(STATE_DIR).context("Failed to create devspace state dir")?;
    let data = serde_json::to_string_pretty(state)?;
    fs::write(STATE_FILE, data).context("Failed to write devspace state")?;
    Ok(())
}

fn resolve_dev_repos(metadata: &Metadata) -> Result<Vec<DevRepo>> {
    let override_map: HashMap<&str, &str> = DEVSPACE_REPO_OVERRIDES.iter().copied().collect();

    DEVSPACE_REPOS
        .iter()
        .map(|crate_name| {
            let override_url = override_map.get(*crate_name).copied();

            let matches: Vec<&Package> = metadata
                .packages
                .iter()
                .filter(|pkg| pkg.name == *crate_name)
                .collect();

            if matches.is_empty() {
                return Err(anyhow!(
                    "crate {crate_name} not found in workspace metadata"
                ));
            }

            let pkg = matches
                .iter()
                .copied()
                .find(|pkg| {
                    pkg.source
                        .as_ref()
                        .map(|src| src.to_string().starts_with("git+"))
                        .unwrap_or(false)
                })
                .unwrap_or(*matches.first().unwrap());

            let source_raw = pkg
                .source
                .as_ref()
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("crate {crate_name} has no source information"))?;

            let (patch_source, git_url) = if source_raw.starts_with("git+") {
                let normalized = normalize_source(&source_raw).ok_or_else(|| {
                    anyhow!(
                        "crate {} has unsupported source {}",
                        crate_name,
                        source_raw.clone()
                    )
                })?;
                let git_url = extract_git_url(&source_raw).ok_or_else(|| {
                    anyhow!(
                        "crate {} has unsupported source {}",
                        crate_name,
                        source_raw.clone()
                    )
                })?;
                (normalized, git_url)
            } else if source_raw == "registry+https://github.com/rust-lang/crates.io-index" {
                let repo_url = if let Some(url) = pkg.repository.clone() {
                    url
                } else if let Some(url) = override_url {
                    println!(
                        "crate {crate_name} is missing repository metadata; using override {url}"
                    );
                    url.to_string()
                } else {
                    return Err(anyhow!(
                        "crate {crate_name} is from crates.io but missing repository metadata"
                    ));
                };
                (CRATES_IO_SOURCE_KEY.to_string(), repo_url)
            } else {
                return Err(anyhow!(
                    "crate {crate_name} uses unsupported source {source_raw}"
                ));
            };

            Ok(DevRepo {
                name: crate_name.to_string(),
                git_url,
                source: patch_source,
                dest: format!("modules/{crate_name}"),
            })
        })
        .collect()
}

fn build_repo_lookup(repos: &[DevRepo]) -> HashMap<String, &DevRepo> {
    repos
        .iter()
        .map(|repo| (repo_lookup_key(&repo.source, &repo.name), repo))
        .collect()
}

fn manifest_relative_path(path: &Path) -> Option<PathBuf> {
    let components: Vec<_> = path.components().collect();
    let idx = components
        .iter()
        .position(|comp| comp.as_os_str() == "checkouts")?;
    if idx + 3 >= components.len() {
        return None;
    }
    let mut rel = PathBuf::new();
    for comp in &components[idx + 3..] {
        rel.push(comp.as_os_str());
    }
    Some(rel)
}

fn manifest_relative_dir(path: &Path) -> Option<PathBuf> {
    let mut rel = manifest_relative_path(path)?;
    if rel.pop() {
        Some(rel)
    } else {
        Some(PathBuf::new())
    }
}

fn normalize_source(raw: &str) -> Option<String> {
    if let Some(trimmed) = raw.strip_prefix("git+") {
        let no_fragment = trimmed.split('#').next().unwrap_or(trimmed);
        let no_query = no_fragment.split('?').next().unwrap_or(no_fragment);
        let without_git = no_query.trim_end_matches(".git");
        let normalized = without_git.trim_end_matches('/');
        Some(normalized.to_string())
    } else if raw == "registry+https://github.com/rust-lang/crates.io-index" {
        Some(CRATES_IO_SOURCE_KEY.to_string())
    } else {
        None
    }
}

fn extract_git_url(raw: &str) -> Option<String> {
    if !raw.starts_with("git+") {
        return None;
    }
    let trimmed = &raw[4..];
    let no_query = trimmed.split('?').next().unwrap_or(trimmed);
    let no_fragment = no_query.split('#').next().unwrap_or(no_query);
    Some(no_fragment.to_string())
}

fn render_devspace_section(specs: &[PatchSpec]) -> String {
    let mut grouped: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for spec in specs {
        grouped
            .entry(spec.source.clone())
            .or_default()
            .insert(spec.crate_name.clone(), spec.path.clone());
    }

    let mut section = String::new();
    section.push_str(PATCH_BEGIN_MARKER);
    section.push('\n');
    section.push_str("# Managed by `cargo xtask devspace`");
    section.push('\n');

    let mut iter = grouped.iter().peekable();
    while let Some((source, crates)) = iter.next() {
        section.push_str(&format!("[patch.\"{source}\"]\n"));
        for (crate_name, path) in crates {
            section.push_str(&format!("{crate_name} = {{ path = \"{path}\" }}\n"));
        }
        if iter.peek().is_some() {
            section.push('\n');
        }
    }

    section.push('\n');
    section.push_str(PATCH_END_MARKER);
    section.push('\n');
    section
}

fn strip_devspace_section(contents: &str) -> (String, bool) {
    if let Some(start_idx) = contents.find(PATCH_BEGIN_MARKER)
        && let Some(end_rel) = contents[start_idx..].find(PATCH_END_MARKER)
    {
        let end_idx = start_idx + end_rel + PATCH_END_MARKER.len();
        let mut removal_end = end_idx;
        let tail = &contents[removal_end..];
        if tail.starts_with("\r\n") {
            removal_end += 2;
        } else if tail.starts_with('\n') {
            removal_end += 1;
        }
        let mut result = String::with_capacity(contents.len());
        result.push_str(&contents[..start_idx]);
        result.push_str(&contents[removal_end..]);
        return (result, true);
    }
    (contents.to_string(), false)
}

fn to_unix_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn run_git(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .current_dir(workspace_root()?)
        .args(args)
        .status()
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!("git command failed: git {}", args.join(" ")));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    std::env::current_dir().context("Failed to resolve workspace root")
}

#[derive(Default, Serialize, Deserialize)]
struct DevspaceState {
    modules: HashMap<String, ManagedModule>,
    patches: Vec<PatchRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ManagedModule {
    name: String,
    path: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct PatchRecord {
    source: String,
    crate_name: String,
}

#[derive(Clone)]
struct PatchSpec {
    source: String,
    crate_name: String,
    path: String,
}

#[derive(Clone)]
struct DevRepo {
    name: String,
    git_url: String,
    source: String,
    dest: String,
}

fn repo_lookup_key(source: &str, crate_name: &str) -> String {
    format!("{source}::{crate_name}")
}
