//! Compose self-contained board checks without changing their boot contract.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use ostool::board::config::BoardRunConfig;

use super::{StarryBoardTestGroup, board_assets::PreparedBoardSessionAssets};

pub(super) struct PreparedBoardCase {
    pub(super) label: String,
    pub(super) group: StarryBoardTestGroup,
    pub(super) config: BoardRunConfig,
    pub(super) config_path: PathBuf,
    pub(super) assets: Option<PreparedBoardSessionAssets>,
}

pub(super) struct PreparedBoardBatch {
    pub(super) build_index: usize,
    pub(super) members: Vec<PreparedBoardCase>,
}

pub(super) fn append_case(
    batches: &mut Vec<PreparedBoardBatch>,
    build_index: usize,
    case: PreparedBoardCase,
) {
    if let Some(batch) = batches.iter_mut().find(|batch| {
        batch.build_index == build_index
            && case.group.shared_boot
            && batch.members[0].group.shared_boot
            && compatible(&batch.members[0].config, &case.config)
    }) {
        batch.members.push(case);
    } else {
        batches.push(PreparedBoardBatch {
            build_index,
            members: vec![case],
        });
    }
}

fn self_contained(config: &BoardRunConfig) -> bool {
    config.timeout.is_some_and(|seconds| seconds > 0)
        && config.shell_check_steps.iter().all(|step| {
            step.shell_cmd
                .as_ref()
                .is_some_and(|command| !command.trim().is_empty())
        })
        && config.shell_check_steps.last().is_some_and(|step| {
            step.success_regex
                .as_ref()
                .is_some_and(|patterns| !patterns.is_empty())
        })
}

fn boot_contract(config: &BoardRunConfig) -> BoardRunConfig {
    let mut config = config.clone();
    config.shell_check_steps.clear();
    config.fail_regex.clear();
    config.session_files.clear();
    config.timeout = None;
    config
}

pub(super) fn compatible(left: &BoardRunConfig, right: &BoardRunConfig) -> bool {
    // Only the leader may contain multiple steps. Its original boot deadline
    // bounds the entire batch, so none of its setup steps renew that budget.
    // Followers must be single commands bounded by their own timeout and the
    // remaining batch time. Missing and explicit DTBs remain distinct.
    self_contained(left)
        && self_contained(right)
        && right.shell_check_steps.len() == 1
        && (left.shell_check_steps.len() == 1 || right.timeout <= left.timeout)
        && boot_contract(left) == boot_contract(right)
}

pub(super) fn combined_config(batch: &PreparedBoardBatch) -> anyhow::Result<BoardRunConfig> {
    let first = batch.members.first().context("empty board batch")?;
    let mut config = first.config.clone();
    if batch.members.len() == 1 {
        return Ok(config);
    }
    config.shell_check_steps.clear();
    config.session_files.clear();
    config.fail_regex.clear();
    let leader_has_multiple_steps = first.config.shell_check_steps.len() > 1;
    let mut total_timeout = 0u64;
    for (index, member) in batch.members.iter().enumerate() {
        ensure!(
            (index == 0 && self_contained(&member.config))
                || (index > 0 && compatible(&first.config, &member.config)),
            "incompatible board batch member {}",
            member.label
        );
        let timeout = member
            .config
            .timeout
            .context("batched board check needs an explicit timeout")?;
        total_timeout = total_timeout
            .checked_add(timeout)
            .context("board batch timeout overflow")?;
        if index == 0 && leader_has_multiple_steps {
            config
                .shell_check_steps
                .extend(member.config.shell_check_steps.iter().cloned());
        } else {
            let mut step = member.config.shell_check_steps[0].clone();
            step.timeout = Some(step.timeout.unwrap_or(timeout).min(timeout));
            config.shell_check_steps.push(step);
        }
        for pattern in &member.config.fail_regex {
            if !config.fail_regex.contains(pattern) {
                config.fail_regex.push(pattern.clone());
            }
        }
    }
    config.timeout = if leader_has_multiple_steps {
        first.config.timeout
    } else {
        Some(total_timeout)
    };
    Ok(config)
}

pub(super) fn merge_uploads(
    source: &Path,
    destination: &Path,
    paths: &[PathBuf],
) -> anyhow::Result<()> {
    for relative in paths {
        super::board_assets::validate_relative_path(relative)?;
        let from = source.join(relative);
        ensure!(
            fs::symlink_metadata(&from)?.is_file(),
            "session asset is not a regular file: {}",
            from.display()
        );
        let to = destination.join(relative);
        if to.exists() {
            ensure!(
                fs::read(&from)? == fs::read(&to)?,
                "board batch session file collision: {}",
                relative.display()
            );
            continue;
        }
        fs::create_dir_all(to.parent().context("session asset has no parent")?)?;
        fs::copy(&from, &to).with_context(|| {
            format!("failed to stage board session file {}", relative.display())
        })?;
    }
    Ok(())
}

pub(super) fn prepare_uploads(
    batch: &PreparedBoardBatch,
    destination: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = BTreeSet::new();
    for member in &batch.members {
        let (source, relative_paths) = match &member.assets {
            Some(assets) => (assets.root.as_path(), assets.relative_paths.as_slice()),
            None => (
                member
                    .config_path
                    .parent()
                    .context("board config has no parent")?,
                member.config.session_files.as_slice(),
            ),
        };
        merge_uploads(source, destination, relative_paths)?;
        paths.extend(relative_paths.iter().cloned());
    }
    // A batch of checks using preinstalled programs legitimately has no uploads.
    Ok(paths.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use ostool::run::ShellCheckStep;

    use super::*;

    fn config(command: &str, marker: &str) -> BoardRunConfig {
        BoardRunConfig {
            board_type: "fixture-board".into(),
            timeout: Some(7),
            shell_check_steps: vec![ShellCheckStep {
                shell_prefix: Some("fixture>".into()),
                shell_cmd: Some(command.into()),
                success_regex: Some(vec![marker.into()]),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn checks_only_share_a_boot_when_their_runtime_contract_is_equal() {
        let left = config("first", "FIRST");
        let mut right = config("second", "SECOND");
        assert!(compatible(&left, &right));
        right.dtb_file = Some("other.dtb".into());
        assert!(!compatible(&left, &right));
        right = config("second", "SECOND");
        right
            .shell_check_steps
            .push(right.shell_check_steps[0].clone());
        assert!(!compatible(&left, &right));
    }

    #[test]
    fn composed_checks_keep_order_deadlines_and_failure_conditions() {
        let make_case = |name: &str, mut config: BoardRunConfig| {
            config.fail_regex = vec![format!("{name}_FAILED"), "PANIC".into()];
            PreparedBoardCase {
                label: name.into(),
                group: StarryBoardTestGroup {
                    name: name.into(),
                    board_name: "fixture".into(),
                    arch: "aarch64".into(),
                    target: "aarch64-unknown-none-softfloat".into(),
                    build_config_path: PathBuf::from("build.toml"),
                    build_input: crate::build::load_config_input(Path::new("."), None, None)
                        .unwrap(),
                    board_test_config_path: PathBuf::from(format!("{name}.toml")),
                    required_env: Vec::new(),
                    shared_boot: true,
                },
                config,
                config_path: PathBuf::from(format!("{name}.toml")),
                assets: None,
            }
        };
        let mut batches = Vec::new();
        append_case(
            &mut batches,
            0,
            make_case("first", config("first-command", "FIRST_OK")),
        );
        let mut second = config("second-command", "SECOND_OK");
        second.timeout = Some(11);
        second.shell_check_steps[0].timeout = Some(3);
        append_case(&mut batches, 0, make_case("second", second));
        assert_eq!(batches.len(), 1);
        let combined = combined_config(&batches[0]).unwrap();
        let upload_root = tempfile::tempdir().unwrap();
        assert!(
            prepare_uploads(&batches[0], upload_root.path())
                .unwrap()
                .is_empty()
        );
        assert_eq!(combined.timeout, Some(18));
        assert_eq!(
            combined
                .shell_check_steps
                .iter()
                .map(|step| step.shell_cmd.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["first-command", "second-command"]
        );
        assert_eq!(combined.shell_check_steps[0].timeout, Some(7));
        assert_eq!(combined.shell_check_steps[1].timeout, Some(3));
        assert_eq!(
            combined.fail_regex,
            ["first_FAILED", "PANIC", "second_FAILED"]
        );
        let mut distinct = config("other-command", "OTHER_OK");
        distinct.boot.cmdline = Some("other-root".into());
        append_case(&mut batches, 0, make_case("other", distinct));
        assert_eq!(batches.len(), 2);
        let mut unverified = make_case("unverified", config("unverified-command", "OK"));
        unverified.group.shared_boot = false;
        append_case(&mut batches, 0, unverified);
        assert_eq!(batches.len(), 3);
    }

    #[test]
    fn multi_step_leader_uses_one_boot_deadline_without_renewing_its_budget() {
        let make_case = |name: &str, config: BoardRunConfig| PreparedBoardCase {
            label: name.into(),
            group: StarryBoardTestGroup {
                name: name.into(),
                board_name: "fixture".into(),
                arch: "aarch64".into(),
                target: "aarch64-unknown-none-softfloat".into(),
                build_config_path: PathBuf::from("build.toml"),
                build_input: crate::build::load_config_input(Path::new("."), None, None).unwrap(),
                board_test_config_path: PathBuf::from(format!("{name}.toml")),
                required_env: Vec::new(),
                shared_boot: true,
            },
            config,
            config_path: PathBuf::from(format!("{name}.toml")),
            assets: None,
        };
        let mut leader = config("run-workflow", "WORKFLOW_OK");
        leader.timeout = Some(12);
        leader.shell_check_steps[0].timeout = Some(3);
        leader.shell_check_steps.insert(
            0,
            ShellCheckStep {
                shell_prefix: Some("fixture>".into()),
                shell_cmd: Some("prepare-workflow".into()),
                ..Default::default()
            },
        );
        let mut batches = Vec::new();
        append_case(&mut batches, 0, make_case("leader", leader.clone()));
        append_case(
            &mut batches,
            0,
            make_case("capture", config("capture", "CAPTURE_OK")),
        );
        assert_eq!(batches.len(), 1);
        let combined = combined_config(&batches[0]).unwrap();
        assert_eq!(combined.timeout, Some(12));
        assert_eq!(combined.shell_check_steps.len(), 3);
        assert_eq!(combined.shell_check_steps[0], leader.shell_check_steps[0]);
        assert_eq!(combined.shell_check_steps[1], leader.shell_check_steps[1]);
        assert_eq!(combined.shell_check_steps[2].timeout, Some(7));

        let mut long = config("longer", "LONGER_OK");
        long.timeout = Some(13);
        append_case(&mut batches, 0, make_case("longer", long));
        assert_eq!(batches.len(), 2);
        append_case(
            &mut batches,
            0,
            make_case("another-workflow", leader.clone()),
        );
        assert_eq!(batches.len(), 3);
        let mut reversed = Vec::new();
        append_case(
            &mut reversed,
            0,
            make_case("capture", config("capture", "CAPTURE_OK")),
        );
        append_case(&mut reversed, 0, make_case("leader", leader));
        assert_eq!(reversed.len(), 2);
    }

    #[test]
    fn session_files_merge_identical_content_and_reject_conflicting_content() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("bin")).unwrap();
        fs::write(source.path().join("bin/helper"), b"original").unwrap();
        let paths = vec![PathBuf::from("bin/helper")];
        merge_uploads(source.path(), destination.path(), &paths).unwrap();
        merge_uploads(source.path(), destination.path(), &paths).unwrap();
        fs::write(source.path().join("bin/helper"), b"changed").unwrap();
        assert!(merge_uploads(source.path(), destination.path(), &paths).is_err());
        assert_eq!(
            fs::read(destination.path().join("bin/helper")).unwrap(),
            b"original"
        );
    }
}
