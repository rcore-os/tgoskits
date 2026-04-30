use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use ostool::{board::RunBoardOptions, build::config::Cargo, run::qemu::QemuConfig};
use serde::Deserialize;

use super::{ArgsTestBoard, ArgsTestQemu, ArgsTestUboot, Starry, board, build, rootfs};
use crate::{
    context::{
        ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs, arch_for_target_checked,
        resolve_starry_arch_and_target, validate_supported_target,
    },
    test::{
        board as board_test, case,
        case::{TestQemuCase, TestQemuSubcase, TestQemuSubcaseKind},
        qemu as qemu_test,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryTestGroup {
    Normal,
    Stress,
}

impl StarryTestGroup {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Stress => "stress",
        }
    }

    pub(crate) fn parse(name: &str) -> anyhow::Result<Self> {
        match name {
            "normal" => Ok(Self::Normal),
            "stress" => Ok(Self::Stress),
            _ => bail!(
                "unsupported Starry test group `{name}`. Supported groups are: normal, stress"
            ),
        }
    }
}

pub(crate) fn resolve_qemu_test_group(
    selected_group: Option<&str>,
    stress: bool,
) -> anyhow::Result<StarryTestGroup> {
    if stress {
        if let Some(group) = selected_group
            && group != StarryTestGroup::Stress.as_str()
        {
            bail!(
                "`--stress` is equivalent to `--test-group stress` and cannot be combined with \
                 `--test-group {group}`"
            );
        }
        return Ok(StarryTestGroup::Stress);
    }

    StarryTestGroup::parse(selected_group.unwrap_or(StarryTestGroup::Normal.as_str()))
}

/// Starry-specific extra fields in a QEMU test case TOML.
///
/// Only the fields that are not part of `ostool`'s `QemuConfig` are read here.
/// The remaining fields (including `shell_init_cmd`) are read as `QemuConfig`
/// later in `prepare_qemu_cases` so we avoid parsing the same file twice.
#[derive(Debug, Deserialize)]
struct StarryQemuCaseConfig {
    #[serde(default)]
    test_commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StarryQemuCaseOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuCaseReport {
    pub(crate) name: String,
    pub(crate) outcome: StarryQemuCaseOutcome,
    pub(crate) duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryQemuRunReport {
    pub(crate) group: StarryTestGroup,
    pub(crate) cases: Vec<StarryQemuCaseReport>,
    pub(crate) total_duration: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryBoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for StarryBoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StarryQemuCaseRequirements {
    smp: usize,
}

#[derive(Debug, Clone)]
struct PreparedStarryQemuCase {
    case: TestQemuCase,
    qemu: QemuConfig,
    requirements: StarryQemuCaseRequirements,
}

struct StarryQemuCaseGroup<'a> {
    requirements: StarryQemuCaseRequirements,
    cases: Vec<&'a PreparedStarryQemuCase>,
}

pub(crate) fn parse_test_target(
    workspace_root: &Path,
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    let supported_targets = board::board_default_list(workspace_root)?
        .into_iter()
        .filter(|board| board.name.starts_with("qemu-"))
        .map(|board| board.target)
        .collect::<Vec<_>>();

    let supported_target_refs = supported_targets
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let supported_arches = supported_targets
        .iter()
        .map(|target| arch_for_target_checked(target))
        .collect::<anyhow::Result<BTreeSet<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();

    let (arch, target) = resolve_starry_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, "starry qemu tests", "arch values", &supported_arches)?;
    validate_supported_target(
        &target,
        "starry qemu tests",
        "targets",
        &supported_target_refs,
    )?;
    Ok((arch, target))
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    arch: &str,
    selected_case: Option<&str>,
    group: StarryTestGroup,
) -> anyhow::Result<Vec<TestQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group);
    qemu_test::discover_qemu_cases(
        &test_suite_dir,
        arch,
        selected_case,
        &format!("Starry {} test case", group.as_str()),
        &format!("Starry {} qemu test cases", group.as_str()),
        load_qemu_case,
        |case| case.name.as_str(),
    )
}

fn load_qemu_case(
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
) -> anyhow::Result<TestQemuCase> {
    let test_commands = load_qemu_case_test_commands(&qemu_config_path)?;
    let subcases = if test_commands.is_empty() {
        Vec::new()
    } else {
        discover_qemu_subcases(&case_dir)?
    };

    Ok(TestQemuCase {
        name,
        case_dir,
        qemu_config_path,
        test_commands,
        subcases,
    })
}

/// Parses `test_commands` from a Starry QEMU case TOML.
///
/// Only the Starry-specific `test_commands` field is read here.  The
/// mutual-exclusion check against `shell_init_cmd` is deferred to
/// `prepare_qemu_cases` where the full `QemuConfig` (including
/// `shell_init_cmd`) is already available, avoiding a second file read.
fn load_qemu_case_test_commands(qemu_config_path: &Path) -> anyhow::Result<Vec<String>> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    let config: StarryQemuCaseConfig = toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))?;

    qemu_test::normalize_qemu_test_commands(qemu_config_path, config.test_commands, "Starry")
}

fn discover_qemu_subcases(case_dir: &Path) -> anyhow::Result<Vec<TestQemuSubcase>> {
    let mut subcases = Vec::new();
    for entry in
        fs::read_dir(case_dir).with_context(|| format!("failed to read {}", case_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let kind = if path.join("c").is_dir() {
            Some(TestQemuSubcaseKind::C)
        } else if path.join("rust").is_dir() {
            Some(TestQemuSubcaseKind::Rust)
        } else {
            None
        };

        if let Some(kind) = kind {
            subcases.push(TestQemuSubcase {
                name,
                case_dir: path,
                kind,
            });
        }
    }
    subcases.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(subcases)
}

pub(crate) fn finalize_qemu_case_run(report: &StarryQemuRunReport) -> anyhow::Result<()> {
    println!("{}", render_qemu_case_summary(report));

    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .map(|case| case.name.clone())
        .collect::<Vec<_>>();

    if failed.is_empty() {
        Ok(())
    } else {
        bail!(
            "starry {} qemu tests failed for {} case(s): {}",
            report.group.as_str(),
            failed.len(),
            failed.join(", ")
        )
    }
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, StarryTestGroup::parse(group)?);
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, selected_board, "Starry", || {
        format!(
            "no Starry board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn test_suite_dir(workspace_root: &Path, group: StarryTestGroup) -> PathBuf {
    workspace_root
        .join("test-suit")
        .join("starryos")
        .join(group.as_str())
}

pub(crate) fn resolve_case_build_config_path(
    case_dir: &Path,
    arch: &str,
    target: &str,
) -> Option<PathBuf> {
    let bare_target = case_dir.join(format!("build-{target}.toml"));
    if bare_target.is_file() {
        return Some(bare_target);
    }

    let dotted_target = case_dir.join(format!(".build-{target}.toml"));
    if dotted_target.is_file() {
        return Some(dotted_target);
    }

    let bare_arch = case_dir.join(format!("build-{arch}.toml"));
    if bare_arch.is_file() {
        return Some(bare_arch);
    }

    let dotted_arch = case_dir.join(format!(".build-{arch}.toml"));
    if dotted_arch.is_file() {
        return Some(dotted_arch);
    }

    None
}

fn render_qemu_case_summary(report: &StarryQemuRunReport) -> String {
    let passed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Passed)
        .collect::<Vec<_>>();
    let failed = report
        .cases
        .iter()
        .filter(|case| case.outcome == StarryQemuCaseOutcome::Failed)
        .collect::<Vec<_>>();

    let mut lines = Vec::new();
    lines.push(format!("starry {} qemu summary:", report.group.as_str()));
    lines.push(format!("passed ({}):", passed.len()));
    if passed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            passed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("failed ({}):", failed.len()));
    if failed.is_empty() {
        lines.push("  <none>".to_string());
    } else {
        lines.extend(
            failed
                .iter()
                .map(|case| format!("  {} ({})", case.name, format_duration(case.duration))),
        );
    }

    lines.push(format!("total: {}", format_duration(report.total_duration)));
    lines.join("\n")
}

fn format_duration(duration: Duration) -> String {
    format!("{:.2}s", duration.as_secs_f64())
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
    {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }

        let case_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        for config_entry in fs::read_dir(&case_dir)
            .with_context(|| format!("failed to read {}", case_dir.display()))?
        {
            let config_entry = config_entry?;
            let config_path = config_entry.path();
            if !config_path.is_file() || config_path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }

            let Some(stem) = config_path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(board_name) = stem.strip_prefix("board-") else {
                continue;
            };

            let default_build_config_path = workspace_root
                .join("os/StarryOS/configs/board")
                .join(format!("{board_name}.toml"));
            if !default_build_config_path.is_file() {
                bail!(
                    "Starry board test group `{case_name}/{board_name}` maps to missing build \
                     config `{}`",
                    default_build_config_path.display()
                );
            }

            let board_file =
                board::load_board_file(&default_build_config_path).with_context(|| {
                    format!(
                        "failed to load mapped Starry build config for board test group \
                         `{case_name}/{board_name}`"
                    )
                })?;
            let build_config_path = resolve_case_build_config_path(
                &case_dir,
                arch_for_target_checked(&board_file.target)?,
                &board_file.target,
            )
            .unwrap_or(default_build_config_path);
            groups.push(StarryBoardTestGroup {
                name: case_name.clone(),
                board_name: board_name.to_string(),
                arch: arch_for_target_checked(&board_file.target)?.to_string(),
                target: board_file.target,
                build_config_path,
                board_test_config_path: config_path,
            });
        }
    }

    Ok(groups)
}

impl Starry {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let (arch, target) =
            parse_test_target(self.app.workspace_root(), &args.arch, &args.target)?;
        let test_group = resolve_qemu_test_group(args.test_group.as_deref(), args.stress)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            &arch,
            args.test_case.as_deref(),
            test_group,
        )?;
        let package = crate::context::STARRY_PACKAGE;

        println!(
            "running starry {} qemu tests for package {} on arch: {} (target: {})",
            test_group.as_str(),
            package,
            arch,
            target
        );

        let default_board = board::default_board_for_target(self.app.workspace_root(), &target)?;
        let mut request = self.prepare_request(
            Self::test_build_args(&target, None),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        if let Some(default_board) = default_board {
            request.plat_dyn = Some(default_board.build_info.plat_dyn);
            request.build_info_override = Some(default_board.build_info);
        } else {
            anyhow::bail!(
                "missing Starry qemu defconfig for target `{target}` in tests; expected a default \
                 qemu board config under os/StarryOS/configs/board"
            );
        }
        let rootfs_path = rootfs::ensure_rootfs_in_target_dir(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
        )
        .await?;
        let cargo = build::load_cargo_config(&request)?;
        let cases = self
            .prepare_qemu_cases(&cargo, cases)
            .await
            .context("failed to load Starry qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut reports = Vec::new();
        let case_groups = Self::group_qemu_cases_by_requirements(&cases);
        let mut completed = 0;
        for group in case_groups {
            let (group_request, group_cargo) =
                Self::qemu_group_build_context(&request, group.requirements)?;
            self.app
                .build(group_cargo.clone(), group_request.build_info_path.clone())
                .await
                .with_context(|| {
                    format!(
                        "failed to build Starry qemu test artifact for {}",
                        Self::qemu_case_requirements_summary(group.requirements)
                    )
                })?;

            for case in group.cases {
                completed += 1;
                let case_name = &case.case.name;
                println!("[{completed}/{total}] starry qemu {case_name}");

                let case_started = Instant::now();
                match self
                    .run_qemu_case(&group_request, &group_cargo, &rootfs_path, case)
                    .await
                    .with_context(|| format!("starry qemu test failed for case `{case_name}`"))
                {
                    Ok(()) => {
                        println!("ok: {case_name}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Passed,
                            duration: case_started.elapsed(),
                        });
                    }
                    Err(err) => {
                        eprintln!("failed: {case_name}: {err:#}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Failed,
                            duration: case_started.elapsed(),
                        });
                    }
                }
            }
        }

        finalize_qemu_case_run(&StarryQemuRunReport {
            group: test_group,
            cases: reports,
            total_duration: suite_started.elapsed(),
        })
    }

    pub(super) async fn test_uboot(&mut self, _args: ArgsTestUboot) -> anyhow::Result<()> {
        qemu_test::unsupported_uboot_test_command("starry")
    }

    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            &args.test_group,
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        let total = groups.len();
        let mut failed = Vec::new();

        for (index, group) in groups.into_iter().enumerate() {
            let group_label = format!("{}/{}", group.name, group.board_name);
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();

            if !board_test_config.exists() {
                eprintln!(
                    "failed: {}: missing board test config `{}`",
                    group_label, board_test_config_summary
                );
                failed.push(group_label);
                continue;
            }

            println!("[{}/{}] starry board {}", index + 1, total, group_label);

            let result = async {
                let request = self.prepare_request(
                    Self::test_board_build_args(&group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let cargo = build::load_cargo_config(&request)?;
                let board_config = self
                    .load_board_config(&cargo, Some(board_test_config.as_path()))
                    .await?;
                self.app
                    .board(
                        cargo,
                        request.build_info_path,
                        board_config,
                        RunBoardOptions {
                            board_type: args.board_type.clone(),
                            server: args.server.clone(),
                            port: args.port,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "starry board test failed for group `{}` (build_config={}, \
                             board_test_config={})",
                            group_label,
                            group.build_config_path.display(),
                            board_test_config_summary
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => println!("ok: {}", group_label),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", group_label, err);
                    failed.push(group_label);
                }
            }
        }

        board_test::finalize_board_test_run("starry", &failed)
    }

    fn test_build_args(target: &str, config: Option<PathBuf>) -> StarryCliArgs {
        StarryCliArgs {
            config,
            arch: None,
            target: Some(target.to_string()),
            smp: None,
            debug: false,
        }
    }

    fn test_board_build_args(group: &StarryBoardTestGroup) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(group.build_config_path.clone()),
            arch: None,
            target: Some(group.target.clone()),
            smp: None,
            debug: false,
        }
    }

    async fn prepare_qemu_cases(
        &mut self,
        cargo: &Cargo,
        cases: Vec<TestQemuCase>,
    ) -> anyhow::Result<Vec<PreparedStarryQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        for case in cases {
            let qemu = self
                .app
                .tool_mut()
                .read_qemu_config_from_path_for_cargo(cargo, &case.qemu_config_path)
                .await
                .with_context(|| {
                    format!("failed to read Starry qemu config for case `{}`", case.name)
                })?;
            qemu_test::validate_grouped_qemu_commands(&qemu, &case, "Starry")?;
            let requirements = Self::qemu_case_requirements(&qemu)
                .with_context(|| format!("failed to read QEMU requirements for `{}`", case.name))?;
            prepared.push(PreparedStarryQemuCase {
                case,
                qemu,
                requirements,
            });
        }

        Ok(prepared)
    }

    fn qemu_case_requirements(qemu: &QemuConfig) -> anyhow::Result<StarryQemuCaseRequirements> {
        Ok(StarryQemuCaseRequirements {
            smp: qemu_test::smp_from_qemu_arg(qemu).unwrap_or(1),
        })
    }

    fn group_qemu_cases_by_requirements(
        cases: &[PreparedStarryQemuCase],
    ) -> Vec<StarryQemuCaseGroup<'_>> {
        let mut groups: Vec<StarryQemuCaseGroup<'_>> = Vec::new();
        for case in cases {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.requirements == case.requirements)
            {
                group.cases.push(case);
            } else {
                groups.push(StarryQemuCaseGroup {
                    requirements: case.requirements,
                    cases: vec![case],
                });
            }
        }

        groups
    }

    fn qemu_group_build_context(
        request: &ResolvedStarryRequest,
        requirements: StarryQemuCaseRequirements,
    ) -> anyhow::Result<(ResolvedStarryRequest, Cargo)> {
        let mut request = request.clone();
        request.smp = Some(requirements.smp);
        let cargo = build::load_cargo_config(&request)?;

        Ok((request, cargo))
    }

    fn qemu_case_requirements_summary(requirements: StarryQemuCaseRequirements) -> String {
        format!("requirements smp={}", requirements.smp)
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        rootfs_path: &Path,
        prepared_case: &PreparedStarryQemuCase,
    ) -> anyhow::Result<()> {
        let case = &prepared_case.case;
        let mut qemu = prepared_case.qemu.clone();
        case::apply_grouped_qemu_config(&mut qemu, case);

        qemu_test::apply_smp_qemu_arg(&mut qemu, Some(prepared_case.requirements.smp));
        qemu_test::apply_timeout_scale(&mut qemu);

        let prepare_started = Instant::now();
        let prepared_assets = case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            case,
            rootfs_path.to_path_buf(),
        )
        .await?;
        println!(
            "  prepare assets: {:.2?} (pipeline={}, cache={})",
            prepare_started.elapsed(),
            prepared_assets.pipeline.as_str(),
            if prepared_assets.cache_hit {
                "hit"
            } else {
                "miss"
            }
        );
        rootfs::patch_rootfs(
            &mut qemu,
            &prepared_assets.rootfs_path,
            rootfs::RootfsPatchMode::EnsureDiskBootNet,
        );
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());

        println!(
            "  qemu config: {} (timeout={})",
            case.qemu_config_path.display(),
            qemu_test::qemu_timeout_summary(&qemu)
        );
        println!("  rootfs: {}", prepared_assets.rootfs_path.display());
        let qemu_started = Instant::now();
        let result = self.app.run_qemu(cargo, qemu).await;
        println!("  qemu run: {:.2?}", qemu_started.elapsed());
        // Remove the per-case rootfs copy immediately after the run so disk
        // usage stays bounded to ~1 active copy at a time rather than
        // accumulating one copy per case.
        case::remove_case_rootfs_copy(prepared_assets.rootfs_copy_to_remove.as_deref());
        case::remove_case_run_dir(prepared_assets.run_dir_to_remove.as_deref());
        result
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn write_board_build_config(root: &Path, board_name: &str, target: &str) {
        let path = root
            .join("os/StarryOS/configs/board")
            .join(format!("{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = [\"qemu\"]\nlog = \
                 \"Info\"\nplat_dyn = false\n"
            ),
        )
        .unwrap();
    }

    fn write_case_build_config(root: &Path, relative_dir: &str, name: &str) -> PathBuf {
        let path = root.join(relative_dir).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "features = [\"qemu\"]\nlog = \"Info\"\n").unwrap();
        path
    }

    fn write_board_test_config(root: &Path, case_name: &str, board_name: &str) -> PathBuf {
        let path = root
            .join("test-suit/starryos/normal")
            .join(case_name)
            .join(format!("board-{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
             \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
             pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = \
             300\n",
        )
        .unwrap();
        path
    }

    #[test]
    fn discovers_board_test_group_and_build_mapping() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        let board_test_config = write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
        assert_eq!(groups[0].arch, "aarch64");
        assert_eq!(groups[0].target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            groups[0].build_config_path,
            root.path()
                .join("os/StarryOS/configs/board/orangepi-5-plus.toml")
        );
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_group_by_case() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");
        write_board_test_config(root.path(), "smoke", "vision-five2");

        let groups =
            discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus", "smoke/vision-five2"]
        );
    }

    #[test]
    fn filters_board_test_groups_by_board() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");
        write_board_test_config(root.path(), "syscall", "orangepi-5-plus");
        write_board_test_config(root.path(), "smoke", "vision-five2");

        let groups =
            discover_board_test_groups(root.path(), "normal", None, Some("orangepi-5-plus"))
                .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus", "syscall/orangepi-5-plus"]
        );
    }

    #[test]
    fn rejects_unknown_board_test_board() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let err =
            discover_board_test_groups(root.path(), "normal", None, Some("unknown")).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported Starry board test board `unknown`")
        );
        assert!(err.to_string().contains("orangepi-5-plus"));
    }

    #[test]
    fn rejects_missing_mapped_board_build_config() {
        let root = tempdir().unwrap();
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let err = discover_board_test_groups(root.path(), "normal", None, None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("smoke/orangepi-5-plus"));
        assert!(err.contains("os/StarryOS/configs/board/orangepi-5-plus.toml"));
    }

    fn write_qemu_test_config(root: &Path, group: StarryTestGroup, case_name: &str, arch: &str) {
        let path = root
            .join("test-suit/starryos")
            .join(group.as_str())
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "timeout = 1\n").unwrap();
    }

    fn write_grouped_qemu_test_config(
        root: &Path,
        group: StarryTestGroup,
        case_name: &str,
        arch: &str,
    ) {
        let path = root
            .join("test-suit/starryos")
            .join(group.as_str())
            .join(case_name)
            .join(format!("qemu-{arch}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "shell_prefix = \"root@starry:\"\ntest_commands = [\"/usr/bin/beta\", \
             \"/usr/bin/alpha\"]\ntimeout = 1\n",
        )
        .unwrap();
    }

    fn prepared_qemu_case(
        name: &str,
        requirements: StarryQemuCaseRequirements,
    ) -> PreparedStarryQemuCase {
        PreparedStarryQemuCase {
            case: TestQemuCase {
                name: name.to_string(),
                case_dir: PathBuf::from(format!("/tmp/{name}")),
                qemu_config_path: PathBuf::from(format!("/tmp/{name}/qemu-x86_64.toml")),
                test_commands: Vec::new(),
                subcases: Vec::new(),
            },
            qemu: QemuConfig::default(),
            requirements,
        }
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_test_config(root.path(), StarryTestGroup::Normal, "smoke", "x86_64");
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/usb")).unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", None, StarryTestGroup::Normal).unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "smoke");
        assert!(cases[0].test_commands.is_empty());
        assert!(cases[0].subcases.is_empty());
        assert_eq!(
            cases[0].case_dir,
            root.path().join("test-suit/starryos/normal/smoke")
        );
    }

    #[test]
    fn discovers_grouped_case_commands_and_sorted_subcases() {
        let root = tempdir().unwrap();
        write_grouped_qemu_test_config(root.path(), StarryTestGroup::Normal, "bugfix", "x86_64");
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/bugfix/beta/c")).unwrap();
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/bugfix/alpha/c")).unwrap();

        let cases =
            discover_qemu_cases(root.path(), "x86_64", None, StarryTestGroup::Normal).unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "bugfix");
        assert_eq!(
            cases[0].test_commands,
            vec!["/usr/bin/beta".to_string(), "/usr/bin/alpha".to_string()]
        );
        assert_eq!(
            cases[0]
                .subcases
                .iter()
                .map(|subcase| subcase.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta"]
        );
        assert!(
            cases[0]
                .subcases
                .iter()
                .all(|subcase| subcase.kind == TestQemuSubcaseKind::C)
        );
    }

    #[test]
    fn grouped_case_loads_with_both_shell_init_cmd_and_test_commands_present() {
        // The mutual-exclusion check has been moved from the initial TOML parse
        // (discover_qemu_cases) to prepare_qemu_cases so we only read each
        // file once.  Therefore, discovery itself should succeed here; the
        // conflict is detected later when QemuConfig is available.
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("test-suit/starryos/normal/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "shell_prefix = \"root@starry:\"\nshell_init_cmd = \"/usr/bin/old\"\ntest_commands = \
             [\"/usr/bin/new\"]\n",
        )
        .unwrap();

        // Discovery no longer validates the shell_init_cmd / test_commands
        // conflict; it should succeed and leave a grouped case behind.
        let cases = discover_qemu_cases(
            root.path(),
            "x86_64",
            Some("bugfix"),
            StarryTestGroup::Normal,
        )
        .unwrap();
        assert_eq!(cases.len(), 1);
        assert!(!cases[0].test_commands.is_empty());
    }

    #[test]
    fn grouped_case_rejects_empty_test_command() {
        let root = tempdir().unwrap();
        let path = root
            .path()
            .join("test-suit/starryos/normal/bugfix/qemu-x86_64.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "test_commands = [\"/usr/bin/ok\", \"  \"]\n").unwrap();

        let err = discover_qemu_cases(
            root.path(),
            "x86_64",
            Some("bugfix"),
            StarryTestGroup::Normal,
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("contains an empty test command"));
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("test-suit/starryos/normal/usb")).unwrap();

        let err = discover_qemu_cases(root.path(), "x86_64", Some("usb"), StarryTestGroup::Normal)
            .unwrap_err()
            .to_string();

        assert!(err.contains("does not provide"));
        assert!(err.contains("qemu-x86_64.toml"));
    }

    #[test]
    fn qemu_case_requirements_read_smp_from_case_config() {
        let qemu = QemuConfig {
            args: vec![
                "-nographic".to_string(),
                "-smp".to_string(),
                "cpus=4".to_string(),
            ],
            ..Default::default()
        };

        let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

        assert_eq!(requirements, StarryQemuCaseRequirements { smp: 4 });
    }

    #[test]
    fn qemu_case_requirements_default_to_single_cpu() {
        let qemu = QemuConfig::default();

        let requirements = Starry::qemu_case_requirements(&qemu).unwrap();

        assert_eq!(requirements, StarryQemuCaseRequirements { smp: 1 });
    }

    #[test]
    fn qemu_cases_are_grouped_by_exact_requirements() {
        let cases = vec![
            prepared_qemu_case("smoke", StarryQemuCaseRequirements { smp: 1 }),
            prepared_qemu_case("affinity", StarryQemuCaseRequirements { smp: 4 }),
            prepared_qemu_case("syscall", StarryQemuCaseRequirements { smp: 1 }),
        ];

        let groups = Starry::group_qemu_cases_by_requirements(&cases);

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].requirements,
            StarryQemuCaseRequirements { smp: 1 }
        );
        assert_eq!(
            groups[0]
                .cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke", "syscall"]
        );
        assert_eq!(
            groups[1].requirements,
            StarryQemuCaseRequirements { smp: 4 }
        );
        assert_eq!(
            groups[1]
                .cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["affinity"]
        );
    }

    #[test]
    fn board_test_group_prefers_case_target_build_config() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");
        let build = write_case_build_config(
            root.path(),
            "test-suit/starryos/normal/smoke",
            "build-aarch64-unknown-none-softfloat.toml",
        );

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups[0].build_config_path, build);
    }

    #[test]
    fn board_test_group_falls_back_to_mapped_board_build_config() {
        let root = tempdir().unwrap();
        write_board_build_config(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_board_test_config(root.path(), "smoke", "orangepi-5-plus");

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(
            groups[0].build_config_path,
            root.path()
                .join("os/StarryOS/configs/board/orangepi-5-plus.toml")
        );
    }

    #[test]
    fn qemu_summary_lists_passed_and_failed_cases() {
        let report = StarryQemuRunReport {
            group: StarryTestGroup::Normal,
            cases: vec![
                StarryQemuCaseReport {
                    name: "smoke".to_string(),
                    outcome: StarryQemuCaseOutcome::Passed,
                    duration: Duration::from_millis(500),
                },
                StarryQemuCaseReport {
                    name: "usb".to_string(),
                    outcome: StarryQemuCaseOutcome::Failed,
                    duration: Duration::from_secs(2),
                },
            ],
            total_duration: Duration::from_secs(3),
        };

        let summary = render_qemu_case_summary(&report);

        assert!(summary.contains("starry normal qemu summary:"));
        assert!(summary.contains("smoke (0.50s)"));
        assert!(summary.contains("usb (2.00s)"));
        assert!(summary.contains("total: 3.00s"));
    }

    #[test]
    fn resolves_stress_alias_as_stress_group() {
        assert_eq!(
            resolve_qemu_test_group(None, true).unwrap(),
            StarryTestGroup::Stress
        );
        assert_eq!(
            resolve_qemu_test_group(Some("stress"), true).unwrap(),
            StarryTestGroup::Stress
        );
    }

    #[test]
    fn rejects_conflicting_stress_alias_and_group() {
        let err = resolve_qemu_test_group(Some("normal"), true).unwrap_err();

        assert!(err.to_string().contains("`--stress` is equivalent"));
        assert!(err.to_string().contains("`--test-group normal`"));
    }
}
