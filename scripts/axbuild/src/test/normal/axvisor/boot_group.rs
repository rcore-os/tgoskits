//! Explicit caller-owned groups of complete QEMU runtime configurations.
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::Deserialize;

use super::checks::{self, CheckReferences, RuntimeChecks};

pub(super) const MANIFEST: &str = "boot-group.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BootGroup {
    pub(super) id: String,
    pub(super) timeout: i64,
    pub(super) cases: Vec<PathBuf>,
}

pub(super) fn is_manifest(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == MANIFEST)
}

pub(super) fn read(path: &Path) -> anyhow::Result<BootGroup> {
    let group: BootGroup = toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("read boot group {}", path.display()))?;
    crate::execution::qemu::validate_selected_case_name(&group.id, "declared", "boot group")?;
    ensure!(
        !group.id.is_empty()
            && group.id.trim() == group.id
            && group
                .id
                .split('/')
                .all(|part| !matches!(part, "" | "." | "..")),
        "invalid boot group ID in {}",
        path.display()
    );
    ensure!(
        group.timeout > 0 && !group.cases.is_empty(),
        "boot group needs positive timeout and nonempty cases in {}",
        path.display()
    );
    let mut seen = BTreeSet::new();
    for member in &group.cases {
        ensure!(
            !member.is_absolute(),
            "boot group member must be relative to {}",
            path.display()
        );
        let file = path
            .parent()
            .context("group has no directory")?
            .join(member);
        ensure!(
            file.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("qemu-")),
            "boot group needs a complete qemu configuration: {}",
            file.display()
        );
        ensure!(
            seen.insert(
                file.canonicalize()
                    .with_context(|| format!("resolve group member {}", file.display()))?
            ),
            "duplicate boot group member {}",
            file.display()
        );
    }
    Ok(group)
}

pub(super) fn member_names(path: &Path) -> anyhow::Result<Vec<String>> {
    let group = read(path)?;
    let mut names = BTreeSet::new();
    group
        .cases
        .iter()
        .map(|member| {
            let file = path
                .parent()
                .context("group has no directory")?
                .join(member);
            let refs = checks::runtime_check_references(&file)?;
            let id = refs
                .id
                .with_context(|| format!("group member needs axbuild.id: {}", file.display()))?;
            let name = id
                .strip_prefix(&format!("{}/", group.id))
                .filter(|name| !name.is_empty() && !name.contains('/'))
                .with_context(|| {
                    format!(
                        "member ID `{id}` must be directly under group `{}`",
                        group.id
                    )
                })?
                .to_string();
            ensure!(
                names.insert(name.clone()),
                "duplicate boot group member ID `{id}`"
            );
            Ok(name)
        })
        .collect()
}

/// Compile one manifest to one native boot configuration and an ordered check plan.
/// Validation covers every member, even when the caller selects only a subset.
pub(super) fn load(path: &Path, selected: Option<&[String]>) -> anyhow::Result<RuntimeChecks> {
    let group = read(path)?;
    let names = member_names(path)?;
    if let Some(selected) = selected {
        ensure!(
            !selected.is_empty() && selected.iter().all(|name| names.contains(name)),
            "unknown or empty boot group selection in {}",
            path.display()
        );
    }
    let directory = path.parent().context("group has no directory")?;
    let mut canonical_boot = None;
    let mut canonical_build = None;
    let mut runtime = None;
    let mut checks = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut completed = BTreeSet::new();
    for (member, name) in group.cases.iter().zip(names) {
        let file = directory.join(member);
        let mut document: toml::Table = toml::from_str(&fs::read_to_string(&file)?)?;
        let refs: CheckReferences = document
            .remove("axbuild")
            .context("member needs axbuild metadata")?
            .try_into()?;
        refs.validate(&file)?;
        ensure!(
            !refs.standalone,
            "group member cannot also declare standalone: {}",
            file.display()
        );
        ensure!(
            refs.checks.is_none(),
            "boot group requires inline complete members: {}",
            file.display()
        );
        ensure!(
            document.contains_key("args"),
            "missing QEMU boot arguments in {}",
            file.display()
        );
        let mut boot = document.clone();
        for key in ["timeout", "shell_check_steps", "host_http_probe"] {
            boot.remove(key);
        }
        if let Some(first) = &canonical_boot {
            ensure!(
                *first == boot,
                "incompatible boot fields in {} (group {})",
                file.display(),
                path.display()
            );
        } else {
            canonical_boot = Some(boot);
            runtime = Some(document.clone());
        }
        let build = build_inputs(&file)?;
        if let Some(first) = &canonical_build {
            ensure!(
                *first == build,
                "incompatible build configuration in {}",
                file.display()
            );
        } else {
            canonical_build = Some(build);
        }
        for sidecar in ["host-initramfs.toml", "guest-builds.toml"] {
            ensure!(
                sidecar_input(directory, sidecar)?
                    == sidecar_input(file.parent().context("member has no directory")?, sidecar)?,
                "incompatible or missing {sidecar} in {} (group {})",
                file.display(),
                path.display()
            );
        }
        // Validate the functional boundary before adding any shared prerequisites.
        let check = checks::inline_check(&file, &document)?;
        checks::total_budget(std::slice::from_ref(&check))?;
        if selected.is_none_or(|selection| selection.contains(&name)) {
            for required in refs.requires {
                checks::expand_check(
                    &file
                        .parent()
                        .context("member has no directory")?
                        .join(required),
                    &mut visiting,
                    &mut completed,
                    &mut checks,
                    true,
                )?;
            }
            checks.push(check);
        }
    }
    let mut runtime = runtime.context("empty boot group")?;
    let total = checks::total_budget(&checks)?;
    runtime.insert(
        "timeout".into(),
        toml::Value::Integer(if selected.is_some() {
            group.timeout.min(total)
        } else {
            group.timeout
        }),
    );
    checks::combine_steps(&mut runtime, &checks)?;
    Ok(RuntimeChecks {
        references: CheckReferences {
            id: Some(group.id),
            ..Default::default()
        },
        runtime,
        checks,
    })
}

fn build_inputs(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    for directory in path.ancestors().skip(1) {
        let entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        let mut builds = entries
            .into_iter()
            .map(|entry| entry.path())
            .filter(|file| {
                file.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with("build-")
                        && name.to_string_lossy().ends_with(".toml")
                })
            })
            .map(|file| file.canonicalize())
            .collect::<Result<Vec<_>, _>>()?;
        if !builds.is_empty() {
            builds.sort();
            return Ok(builds);
        }
    }
    anyhow::bail!("no build configuration for {}", path.display())
}

fn sidecar_input(directory: &Path, name: &str) -> anyhow::Result<Option<toml::Table>> {
    let path = directory.join(name);
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut document: toml::Table = toml::from_str(&content)?;
    if name == "guest-builds.toml"
        && let Some(paths) = document
            .get_mut("arceos_build_configs")
            .and_then(toml::Value::as_array_mut)
    {
        for path in paths {
            let resolved = directory
                .join(path.as_str().context("guest build path must be a string")?)
                .canonicalize()?;
            *path = toml::Value::String(resolved.to_string_lossy().into_owned());
        }
    }
    Ok(Some(document))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_members_share_one_boot_preserve_dependencies_and_reject_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("fixture");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("build-aarch64-unknown-none-softfloat.toml"),
            "target='aarch64-unknown-none-softfloat'\n",
        )
        .unwrap();
        let manifest = directory.join(MANIFEST);
        fs::write(
            &manifest,
            "id='qemu/aarch64/fixture'\ntimeout=100\ncases=['first/qemu-aarch64.toml','second/\
             qemu-aarch64.toml']\n",
        )
        .unwrap();
        fs::write(
            directory.join("prepare.toml"),
            "timeout=10\n[[shell_check_steps]]\nshell_cmd='prepare'\n",
        )
        .unwrap();
        fs::write(directory.join("host-initramfs.toml"), "vm_configs=[]\n").unwrap();
        for (name, budget) in [("first", 20), ("second", 30)] {
            let member = directory.join(name);
            fs::create_dir(&member).unwrap();
            fs::write(member.join("host-initramfs.toml"), "vm_configs=[]\n").unwrap();
            fs::write(
                member.join("qemu-aarch64.toml"),
                format!(
                    r#"args=[]
timeout={budget}
[[shell_check_steps]]
shell_cmd='{name}'
success_regex=['PASS']
[axbuild]
id='qemu/aarch64/fixture/{name}'
requires=['../prepare.toml']
"#
                ),
            )
            .unwrap();
        }
        let plan = load(&manifest, None).unwrap();
        assert_eq!(plan.runtime["timeout"].as_integer(), Some(100));
        assert_eq!(
            plan.checks
                .iter()
                .map(|check| check.prerequisite)
                .collect::<Vec<_>>(),
            [true, false, false]
        );
        let commands = plan.runtime["shell_check_steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["shell_cmd"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(commands, ["prepare", "first", "second"]);
        let selected = load(&manifest, Some(&["second".into()])).unwrap();
        assert_eq!(selected.runtime["timeout"].as_integer(), Some(40));
        assert_eq!(selected.checks.len(), 2);
        let standalone =
            checks::load_runtime_checks(&directory.join("second/qemu-aarch64.toml")).unwrap();
        assert_eq!(standalone.runtime["timeout"].as_integer(), Some(40));
        assert!(load(&manifest, Some(&["missing".into()])).is_err());
        let discovered = super::super::discovery::discover_qemu_cases(
            super::super::discovery::CaseSource::External(root.path()),
            root.path(),
            ".",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            Some("qemu/aarch64/fixture"),
        )
        .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].case.qemu_config_path, manifest);
        let originals = ["first", "second"].map(|name| {
            let path = directory.join(name).join("qemu-aarch64.toml");
            let contents = fs::read_to_string(&path).unwrap();
            fs::write(&path, format!("rootfs_write_policy='persist'\n{contents}")).unwrap();
            (path, contents)
        });
        assert!(
            super::super::discovery::discover_qemu_cases(
                super::super::discovery::CaseSource::External(root.path()),
                root.path(),
                ".",
                "aarch64",
                "aarch64-unknown-none-softfloat",
                None,
            )
            .err()
            .unwrap()
            .to_string()
            .contains("persist")
        );
        for (path, contents) in originals {
            fs::write(path, contents).unwrap();
        }
        let original_manifest = fs::read_to_string(&manifest).unwrap();
        fs::write(
            &manifest,
            original_manifest.replace(",'second/qemu-aarch64.toml'", ""),
        )
        .unwrap();
        assert!(
            super::super::discovery::discover_qemu_cases(
                super::super::discovery::CaseSource::External(root.path()),
                root.path(),
                ".",
                "aarch64",
                "aarch64-unknown-none-softfloat",
                None
            )
            .is_err()
        );
        fs::write(&manifest, &original_manifest).unwrap();
        let member = directory.join("second/qemu-aarch64.toml");
        let valid = fs::read_to_string(&member).unwrap();
        fs::write(&member, valid.replace("args=[]", "args=['-m','1G']")).unwrap();
        assert!(
            load(&manifest, None)
                .err()
                .unwrap()
                .to_string()
                .contains("incompatible boot")
        );
        fs::write(&member, &valid).unwrap();
        fs::write(
            directory.join("second/host-initramfs.toml"),
            "vm_configs=['guest.toml']\n",
        )
        .unwrap();
        assert!(
            load(&manifest, None)
                .err()
                .unwrap()
                .to_string()
                .contains("host-initramfs")
        );
        fs::write(
            directory.join("second/host-initramfs.toml"),
            "vm_configs=[]\n",
        )
        .unwrap();
        fs::write(&member, valid.replace("fixture/second", "other/second")).unwrap();
        assert!(load(&manifest, None).is_err());
    }
}
