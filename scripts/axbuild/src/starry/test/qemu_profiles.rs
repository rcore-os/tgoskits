use std::{collections::BTreeSet, fs};

use anyhow::{Context, bail};
use serde::Deserialize;

use super::StarryQemuCase;
use crate::test::qemu;

/// Partition a grouped suite into disjoint emulator configurations without
/// changing its source directories or the user's subcase selectors.
pub(super) fn expand(mut base: StarryQemuCase) -> anyhow::Result<Vec<StarryQemuCase>> {
    let path = &base.case.qemu_config_path;
    let config: ProfileConfig = toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("failed to read grouped QEMU profiles in {}", path.display()))?;
    if config.grouped_qemu_profiles.is_empty() {
        return Ok(vec![base]);
    }
    if !base.case.is_grouped() {
        bail!("grouped QEMU profiles require test_commands");
    }
    let selected = base.case.grouped_subcase_filter.clone().unwrap_or_else(|| {
        base.case
            .subcases
            .iter()
            .map(|case| case.name.clone())
            .collect()
    });
    let mut assigned = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut profiles = Vec::new();
    for profile in config.grouped_qemu_profiles {
        if profile.name.is_empty()
            || !profile
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !names.insert(profile.name.clone())
            || profile.subcase_prefix.is_empty()
        {
            bail!("invalid or duplicate grouped QEMU profile {}", profile.name);
        }
        let matching = base
            .case
            .subcases
            .iter()
            .filter(|case| case.name.starts_with(&profile.subcase_prefix))
            .map(|case| case.name.clone())
            .collect::<BTreeSet<_>>();
        if matching.is_empty() {
            bail!("grouped QEMU profile {} matches no subcases", profile.name);
        }
        for name in &matching {
            if !assigned.insert(name.clone()) {
                bail!("subcase {name} belongs to multiple grouped QEMU profiles");
            }
        }
        let mut variant = base.clone();
        let path = base.case.case_dir.join(&profile.config);
        variant.case = qemu::load_test_qemu_case_fields(
            format!("{}-{}", base.case.display_name, profile.name),
            format!("{}-{}", base.case.name, profile.name),
            base.case.case_dir.clone(),
            path,
            "Starry",
            true,
        )?;
        if !variant.case.is_grouped() {
            bail!(
                "grouped QEMU profile {} requires test_commands",
                profile.name
            );
        }
        let filter = matching
            .intersection(&selected)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !filter.is_empty() {
            variant.case.grouped_subcase_filter = Some(filter);
            profiles.push(variant);
        }
    }
    let remaining = selected
        .difference(&assigned)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !remaining.is_empty() {
        base.case.grouped_subcase_filter = Some(remaining);
        profiles.insert(0, base);
    }
    Ok(profiles)
}

#[derive(Deserialize)]
struct ProfileConfig {
    #[serde(default)]
    grouped_qemu_profiles: Vec<GroupedQemuProfile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupedQemuProfile {
    name: String,
    subcase_prefix: String,
    config: String,
}
