use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};

use crate::test::qemu;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardRuntimeConfig {
    pub(crate) case_dir: PathBuf,
    pub(crate) board_name: String,
    pub(crate) config_path: PathBuf,
}

pub(crate) trait BoardTestGroupInfo {
    fn name(&self) -> &str;
    fn board_name(&self) -> &str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardCaseBuildInfo {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

pub(crate) fn labeled_board_cases<T: BoardTestGroupInfo>(groups: Vec<T>) -> Vec<(String, String)> {
    groups
        .into_iter()
        .map(|group| (group.name().to_string(), group.board_name().to_string()))
        .collect()
}

pub(crate) fn filter_board_test_groups<T: BoardTestGroupInfo>(
    mut groups: Vec<T>,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
    suite_name: &str,
    empty_message: impl FnOnce() -> String,
) -> anyhow::Result<Vec<T>> {
    groups.sort_by(|left, right| {
        left.name()
            .cmp(right.name())
            .then_with(|| left.board_name().cmp(right.board_name()))
    });

    if let Some(case_name) = selected_case {
        if groups.is_empty() {
            bail!("{}", empty_message());
        }
        let available = available_values(groups.iter().map(BoardTestGroupInfo::name));
        groups.retain(|group| group.name() == case_name);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported {suite_name} board test case `{case_name}`. Supported cases are: \
                 {available}",
            ));
        }
    }

    if let Some(board_name) = selected_board {
        if groups.is_empty() {
            bail!("{}", empty_message());
        }
        let available = available_values(groups.iter().map(BoardTestGroupInfo::board_name));
        groups.retain(|group| group.board_name() == board_name);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported {suite_name} board test board `{board_name}`. Supported boards are: \
                 {available}",
            ));
        }
    }

    if groups.is_empty() {
        bail!("{}", empty_message());
    }

    Ok(groups)
}

pub(crate) fn discover_board_runtime_configs(
    test_group_dir: &Path,
) -> anyhow::Result<Vec<BoardRuntimeConfig>> {
    let mut configs = Vec::new();
    let mut stack = fs::read_dir(test_group_dir)
        .with_context(|| format!("failed to read {}", test_group_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let path = entry.path();
        if path.is_dir() {
            stack.extend(
                fs::read_dir(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?
                    .collect::<Result<Vec<_>, _>>()?,
            );
            continue;
        }

        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(board_name) = stem.strip_prefix("board-") else {
            continue;
        };
        let Some(case_dir) = path.parent() else {
            continue;
        };
        configs.push(BoardRuntimeConfig {
            case_dir: case_dir.to_path_buf(),
            board_name: board_name.to_string(),
            config_path: path,
        });
    }

    configs.sort_by(|left, right| {
        left.case_dir
            .cmp(&right.case_dir)
            .then_with(|| left.board_name.cmp(&right.board_name))
    });
    Ok(configs)
}

pub(crate) fn discover_board_case_build_infos(
    test_group_dir: &Path,
    suite_name: &str,
) -> anyhow::Result<Vec<BoardCaseBuildInfo>> {
    let mut groups = Vec::new();
    for config in discover_board_runtime_configs(test_group_dir)? {
        let wrapper =
            qemu::nearest_build_wrapper(test_group_dir, &config.case_dir, suite_name, "board")?;
        groups.push(BoardCaseBuildInfo {
            name: qemu::case_name_from_wrapper(test_group_dir, &wrapper, &config.case_dir)?,
            board_name: config.board_name,
            build_config_path: wrapper.build_config_path,
            board_test_config_path: config.config_path,
        });
    }

    Ok(groups)
}

fn available_values<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let available = values
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    if available.is_empty() {
        "<none>".to_string()
    } else {
        available
    }
}

pub(crate) fn finalize_board_test_run(suite_name: &str, failed: &[String]) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {suite_name} board test groups passed");
        Ok(())
    } else {
        bail!(
            "{suite_name} board tests failed for {} group(s): {}",
            failed.len(),
            failed.join(", ")
        )
    }
}

pub(crate) struct BoardTestRunState<'a> {
    suite_name: &'a str,
    total: usize,
    failed: Vec<String>,
}

impl<'a> BoardTestRunState<'a> {
    pub(crate) fn new(suite_name: &'a str, total: usize) -> Self {
        Self {
            suite_name,
            total,
            failed: Vec::new(),
        }
    }

    pub(crate) fn start_group<T: BoardTestGroupInfo>(&self, index: usize, group: &T) -> String {
        let group_label = format!("{}/{}", group.name(), group.board_name());
        println!(
            "[{}/{}] {} board {}",
            index + 1,
            self.total,
            self.suite_name,
            group_label
        );
        group_label
    }

    pub(crate) fn pass_group(&self, group_label: &str) {
        println!("ok: {group_label}");
    }

    pub(crate) fn fail_group(&mut self, group_label: String, err: anyhow::Error) {
        eprintln!("failed: {}: {:#}", group_label, err);
        self.failed.push(group_label);
    }

    pub(crate) fn finish(self) -> anyhow::Result<()> {
        finalize_board_test_run(self.suite_name, &self.failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_board_runtime_configs_recursively() {
        let root = tempfile::tempdir().unwrap();
        let case_dir = root.path().join("normal/case-a");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("board-orangepi-5-plus.toml"), "").unwrap();
        fs::write(case_dir.join("qemu-aarch64.toml"), "").unwrap();

        let nested_case_dir = root.path().join("normal/wrapper/case-b");
        fs::create_dir_all(&nested_case_dir).unwrap();
        fs::write(nested_case_dir.join("board-phytiumpi.toml"), "").unwrap();

        let configs = discover_board_runtime_configs(&root.path().join("normal")).unwrap();

        assert_eq!(
            configs
                .iter()
                .map(|config| config.board_name.as_str())
                .collect::<Vec<_>>(),
            ["orangepi-5-plus", "phytiumpi"]
        );
        assert_eq!(configs[0].case_dir, case_dir);
        assert_eq!(configs[1].case_dir, nested_case_dir);
    }

    #[test]
    fn filter_selected_board_on_empty_group_reports_empty_group() {
        let err = filter_board_test_groups(
            Vec::<TestBoardGroup>::new(),
            None,
            Some("orangepi-5-plus"),
            "Starry",
            || "no Starry board test groups found under /tmp/stress".to_string(),
        )
        .unwrap_err()
        .to_string();

        assert_eq!(err, "no Starry board test groups found under /tmp/stress");
    }

    #[derive(Debug)]
    struct TestBoardGroup {
        name: String,
        board_name: String,
    }

    impl BoardTestGroupInfo for TestBoardGroup {
        fn name(&self) -> &str {
            &self.name
        }

        fn board_name(&self) -> &str {
            &self.board_name
        }
    }
}
