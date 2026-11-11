use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};

use super::catalog::{self, Catalog, Image};
use crate::{context::ResolvedBuildRequest, test::normal::arceos::types::ArceosBoardTestGroup};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardSelection {
    pub(crate) suite: PathBuf,
    pub(crate) batch: String,
    pub(crate) order: usize,
    pub(crate) members: Vec<String>,
    pub(crate) selected: Vec<String>,
    features: Vec<String>,
    image: Image,
}
impl BoardSelection {
    fn selection(&self) -> String {
        catalog::selection(&self.selected, &self.members)
    }
    pub(crate) fn completion(&self) -> String {
        catalog::completion(&self.batch, &self.selection())
    }
}

pub(super) fn collect(root: &Path) -> anyhow::Result<Vec<ArceosBoardTestGroup>> {
    let path = root.join("suite.toml").canonicalize()?;
    let suite = Catalog::load(&path)?;
    let mut groups = Vec::new();
    for (order, id) in suite.run_order.iter().enumerate() {
        let batch = &suite.batches[id];
        let build_path = root.join(&batch.build_config);
        let build_input = crate::build::load_config_input::<crate::arceos::config::ArceosConfig>(
            &crate::context::workspace_root_path()?,
            Some(&build_path),
            None,
        )?;
        let build = &build_input.config;
        let target = build
            .common
            .target
            .clone()
            .context("declared board build requires target")?;
        ensure!(
            build.package.as_deref() == Some(&batch.package)
                && crate::context::arch_for_target_checked(&target)? == batch.arch,
            "declared board package or architecture differs from build configuration"
        );
        ensure!(
            build.freestanding == (batch.image == Image::Freestanding),
            "declared board runtime differs from build configuration"
        );
        ensure!(
            root.join(&batch.board_config).is_file(),
            "declared board runtime configuration is missing"
        );
        let members: Vec<_> = batch
            .select(id, &[])?
            .iter()
            .map(|test| test.id.clone())
            .collect();
        groups.push(ArceosBoardTestGroup {
            name: id.clone(),
            board_name: suite.board.clone(),
            package: batch.package.clone(),
            arch: batch.arch.clone(),
            target,
            build_config_path: build_path,
            build_input,
            board_test_config_path: root.join(&batch.board_config),
            declaration: Some(BoardSelection {
                suite: path.clone(),
                batch: id.clone(),
                order,
                selected: members.clone(),
                members,
                features: batch.features(),
                image: batch.image,
            }),
        });
    }
    Ok(groups)
}

pub(super) fn select(
    mut groups: Vec<ArceosBoardTestGroup>,
    requested: &[String],
) -> anyhow::Result<Vec<ArceosBoardTestGroup>> {
    let available: BTreeSet<_> = groups
        .iter()
        .flat_map(|group| {
            std::iter::once(group.name.as_str()).chain(
                group
                    .declaration
                    .iter()
                    .flat_map(|decl| decl.members.iter().map(String::as_str)),
            )
        })
        .collect();
    let unknown: Vec<_> = requested
        .iter()
        .filter(|id| !available.contains(id.as_str()))
        .collect();
    ensure!(
        unknown.is_empty(),
        "unsupported ArceOS board test case(s): {unknown:?}; available: {available:?}"
    );
    groups.retain_mut(|group| {
        if requested.is_empty() || requested.contains(&group.name) {
            return true;
        }
        let Some(declaration) = &mut group.declaration else {
            return false;
        };
        declaration.selected.retain(|id| requested.contains(id));
        !declaration.selected.is_empty()
    });
    groups.sort_by_key(|group| {
        group
            .declaration
            .as_ref()
            .map_or(usize::MAX, |decl| decl.order)
    });
    Ok(groups)
}

pub(super) fn configure(
    group: &ArceosBoardTestGroup,
    request: &ResolvedBuildRequest,
    cargo: &mut ostool::build::config::Cargo,
) -> anyhow::Result<()> {
    let Some(declaration) = &group.declaration else {
        return Ok(());
    };
    ensure!(
        cargo.package == group.package && request.arch == group.arch,
        "board declaration and resolved build identity disagree"
    );
    ensure!(
        request.config.freestanding == (declaration.image == Image::Freestanding),
        "board image and build runtime disagree"
    );
    cargo.features.extend(declaration.features.clone());
    cargo.features.sort();
    cargo.features.dedup();
    cargo.env.extend(catalog::build_env(
        &declaration.suite,
        &declaration.batch,
        &declaration.selection(),
    ));
    Ok(())
}

/// Rebinds only a passive completion check; interactive scripts cannot be discarded.
pub(super) fn configure_completion(
    group: &ArceosBoardTestGroup,
    config: &mut ostool::board::config::BoardRunConfig,
) -> anyhow::Result<()> {
    let Some(declaration) = &group.declaration else {
        return Ok(());
    };
    ensure!(
        config.shell_check_steps.len() == 1,
        "declared board batches require one passive completion step"
    );
    let step = &mut config.shell_check_steps[0];
    ensure!(
        step.shell_cmd.is_none() && step.shell_prefix.is_none(),
        "declared board batch cannot discard interactive shell steps"
    );
    step.success_regex = Some(vec![format!(
        r"(?m)^{}\s*$",
        regex::escape(&declaration.completion())
    )]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const BUILD: &str = r#"package = "fixture"
target = "aarch64-unknown-none-softfloat"
features = []
log = "Info"
max_cpu_num = 8
"#;

    fn fixture(root: &Path) -> Vec<ArceosBoardTestGroup> {
        fs::create_dir_all(root.join("combined")).unwrap();
        fs::write(root.join("combined/build.toml"), BUILD).unwrap();
        fs::write(
            root.join("combined/board.toml"),
            "board_type = \"Fixture\"\n",
        )
        .unwrap();
        fs::write(
            root.join("suite.toml"),
            r#"
schema_version = 1
board = "fixture"
run_order = ["combined"]
[batches.combined]
arch = "aarch64"
image = "std"
executor = "rust"
package = "fixture"
build_config = "combined/build.toml"
board_config = "combined/board.toml"
[[batches.combined.tests]]
id = "counter"
name = "counter capability"
entry = "cpu_tests::counter::run"
returns = "unit"
features = ["counter-feature"]
[[batches.combined.tests]]
id = "delivery"
name = "delivery capability"
entry = "crate::delivery::run"
returns = "result"
features = ["delivery-feature"]
"#,
        )
        .unwrap();
        collect(root).unwrap()
    }
    #[test]
    fn discovery_selection_and_build_identity_share_one_batch() {
        let directory = tempfile::tempdir().unwrap();
        let groups = fixture(directory.path());
        let full = select(
            groups.clone(),
            &["delivery".into(), "counter".into(), "delivery".into()],
        )
        .unwrap();
        assert_eq!(full.len(), 1);
        assert_eq!(
            full[0].declaration.as_ref().unwrap().completion(),
            "BOARD_BATCH_OK batch=combined selection=all"
        );
        let single = select(groups.clone(), &["delivery".into()]).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(
            single[0].declaration.as_ref().unwrap().selected,
            ["delivery"]
        );
        assert!(select(groups, &["unknown".into()]).is_err());
        let group = &single[0];
        let mut request = crate::context::test_support::arceos_request(
            &group.build_config_path,
            &group.package,
            &group.arch,
            &group.target,
        );
        let mut cargo = ostool::build::config::Cargo {
            package: group.package.clone(),
            target: group.target.clone(),
            ..Default::default()
        };
        configure(group, &request, &mut cargo).unwrap();
        assert_eq!(cargo.features, ["counter-feature", "delivery-feature"]);
        assert_eq!(cargo.env[catalog::CASES_ENV], "delivery");
        let before = cargo.clone();
        configure(&full[0], &request, &mut cargo).unwrap();
        assert_eq!(cargo.features, before.features);
        assert_ne!(cargo.env, before.env);
        assert_eq!(cargo.env[catalog::CASES_ENV], "");
        request.config.freestanding = true;
        let error = configure(group, &request, &mut cargo).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("board image and build runtime disagree")
        );
    }
    #[test]
    fn completion_binding_preserves_failure_checks_and_rejects_interactive_scripts() {
        let directory = tempfile::tempdir().unwrap();
        let group = fixture(directory.path()).remove(0);
        let mut config = ostool::board::config::BoardRunConfig {
            fail_regex: vec!["fatal".into()],
            shell_check_steps: vec![ostool::run::ShellCheckStep {
                success_regex: Some(vec!["old".into()]),
                fail_regex: Some(vec!["bad".into()]),
                ..Default::default()
            }],
            ..Default::default()
        };
        configure_completion(&group, &mut config).unwrap();
        assert_eq!(config.fail_regex, ["fatal"]);
        assert_eq!(
            config.shell_check_steps[0].fail_regex,
            Some(vec!["bad".into()])
        );
        config.shell_check_steps[0].shell_cmd = Some("required operation".into());
        assert!(configure_completion(&group, &mut config).is_err());
        config.shell_check_steps.clear();
        assert!(configure_completion(&group, &mut config).is_err());
    }
}
