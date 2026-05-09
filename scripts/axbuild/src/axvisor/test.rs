use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use ostool::{
    board::RunBoardOptions,
    build::config::Cargo,
    run::{qemu::QemuConfig, uboot::UbootConfig},
};
use serde::Deserialize;

use super::{
    ArgsTest, ArgsTestBoard, ArgsTestQemu, ArgsTestUboot, Axvisor, TestCommand, build, rootfs,
};
use crate::{
    context::{
        AxvisorCliArgs, ResolvedAxvisorRequest, SnapshotPersistence,
        resolve_axvisor_arch_and_target,
    },
    test::{
        board as board_test, case as test_case, case::TestQemuCase, qemu as test_qemu,
        qemu::parse_test_target, suite as test_suite,
    },
};

const AXVISOR_TEST_SUITE_OS: &str = "axvisor";
const AXVISOR_NORMAL_GROUP: &str = "normal";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxvisorQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedAxvisorQemuCase {
    case: AxvisorQemuCase,
    qemu: QemuConfig,
}

impl test_qemu::BuildConfigRef for PreparedAxvisorQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

const TEST_ARCHES: &[&str] = &["aarch64", "riscv64", "x86_64", "loongarch64"];
const TEST_TARGETS: &[&str] = &[
    "aarch64-unknown-none-softfloat",
    "riscv64gc-unknown-none-elf",
    "x86_64-unknown-none",
    "loongarch64-unknown-none-softfloat",
];

pub(super) async fn test(axvisor: &mut Axvisor, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => axvisor.test_qemu(args).await,
        TestCommand::Uboot(args) => axvisor.test_uboot(args).await,
        TestCommand::Board(args) => axvisor.test_board(args).await,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for BoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

#[derive(Debug, Deserialize)]
struct AxvisorQemuCaseConfig {
    // Note: shell_init_cmd is NOT duplicated here; it is read by ostool's
    // QemuConfig during the run phase. Keeping it out avoids two independent
    // sources of truth and makes the mutual-exclusion check with test_commands
    // happen in one place (Axvisor::load_qemu_case_config).
    #[serde(default)]
    test_commands: Vec<String>,
}

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    parse_test_target(
        arch,
        target,
        "axvisor qemu tests",
        TEST_ARCHES,
        TEST_TARGETS,
        resolve_axvisor_arch_and_target,
    )
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    test_qemu::discover_qemu_cases(
        &test_suite_dir,
        arch,
        target,
        selected_case,
        "Axvisor",
        "qemu",
    )?
    .into_iter()
    .map(load_qemu_case)
    .collect()
}

fn load_qemu_case(case: test_qemu::DiscoveredQemuCase) -> anyhow::Result<AxvisorQemuCase> {
    let config = load_qemu_case_config(&case.qemu_config_path)?;
    let test_commands = qemu_case_test_commands(&case.qemu_config_path, &config)?;

    Ok(AxvisorQemuCase {
        case: TestQemuCase {
            display_name: case.display_name,
            name: case.name,
            case_dir: case.case_dir,
            qemu_config_path: case.qemu_config_path,
            test_commands,
            subcases: Vec::new(),
        },
        build_group: case.build_group,
        build_config_path: case.build_config_path,
    })
}

fn load_qemu_case_config(qemu_config_path: &Path) -> anyhow::Result<AxvisorQemuCaseConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))
}

fn qemu_case_test_commands(
    qemu_config_path: &Path,
    config: &AxvisorQemuCaseConfig,
) -> anyhow::Result<Vec<String>> {
    test_qemu::normalize_qemu_test_commands(qemu_config_path, &config.test_commands, "Axvisor")
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    board: Option<&str>,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, board, "axvisor", || {
        format!(
            "no Axvisor board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let mut groups = Vec::new();
    for config in board_test::discover_board_runtime_configs(test_suite_dir)? {
        ensure_board_run_config(&config.config_path)?;
        let wrapper =
            test_qemu::nearest_build_wrapper(test_suite_dir, &config.case_dir, "Axvisor", "board")?;
        let name = test_qemu::case_name_from_wrapper(test_suite_dir, &wrapper, &config.case_dir)?;
        let build_config = resolve_workspace_path(workspace_root, wrapper.build_config_path);
        ensure_file_exists(&build_config, "Axvisor board build group config")?;
        groups.push(BoardTestGroup {
            name,
            board_name: config.board_name,
            build_config,
            board_test_config_path: config.config_path,
        });
    }

    Ok(groups)
}

fn discover_uboot_test_group(
    workspace_root: &Path,
    board: &str,
    guest: &str,
) -> anyhow::Result<BoardTestGroup> {
    let board_name = format!("{board}-{guest}");
    let mut groups = discover_board_test_groups(
        workspace_root,
        AXVISOR_NORMAL_GROUP,
        None,
        Some(&board_name),
    )?;

    if groups.len() == 1 {
        return Ok(groups.remove(0));
    }

    let labels = groups
        .iter()
        .map(|group| format!("{}/{}", group.name, group.board_name))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "ambiguous axvisor uboot test target board=`{board}` guest=`{guest}`. Matching cases are: \
         {labels}"
    )
}

fn merge_board_test_uboot_config(
    base: Option<UbootConfig>,
    board_test: ostool::board::config::BoardRunConfig,
) -> UbootConfig {
    let mut uboot = base.unwrap_or_default();
    let test_uboot = UbootConfig::from_board_run_config(&board_test);
    if test_uboot.dtb_file.is_some() {
        uboot.dtb_file = test_uboot.dtb_file;
    }
    uboot.success_regex = test_uboot.success_regex;
    uboot.fail_regex = test_uboot.fail_regex;
    uboot.uboot_cmd = test_uboot.uboot_cmd;
    uboot.shell_prefix = test_uboot.shell_prefix;
    uboot.shell_init_cmd = test_uboot.shell_init_cmd;
    if test_uboot.timeout.is_some() {
        uboot.timeout = test_uboot.timeout;
    }
    uboot
}

fn ensure_board_run_config(path: &Path) -> anyhow::Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str::<ostool::board::config::BoardRunConfig>(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(())
}

fn resolve_workspace_path(workspace_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn ensure_file_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("{label} maps to missing file `{}`", path.display())
    }
}

fn test_suite_dir(workspace_root: &Path, group: &str) -> anyhow::Result<PathBuf> {
    test_suite::require_group_dir(workspace_root, AXVISOR_TEST_SUITE_OS, "Axvisor", group)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    test_suite::suite_root(workspace_root, AXVISOR_TEST_SUITE_OS)
}

fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    test_suite::discover_group_names(workspace_root, AXVISOR_TEST_SUITE_OS)
}

impl Axvisor {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        if args.list && args.arch.is_none() && args.target.is_none() && args.test_group.is_none() {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    let test_suite_dir = match test_suite_dir(self.app.workspace_root(), &group) {
                        Ok(dir) => dir,
                        Err(err) => return Some(Err(err)),
                    };
                    match test_qemu::discover_all_qemu_cases_with_archs(
                        &test_suite_dir,
                        args.test_case.as_deref(),
                        "Axvisor",
                        &group,
                    ) {
                        Ok(case_names) => Some(Ok((group, case_names))),
                        Err(err) => {
                            let message = err.to_string();
                            if message.starts_with("no Axvisor ")
                                || message.starts_with("unknown Axvisor ")
                            {
                                None
                            } else {
                                Some(Err(err))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                bail!(
                    "no Axvisor qemu test cases found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!("{}", test_qemu::render_qemu_case_forest("axvisor", groups));
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(AXVISOR_NORMAL_GROUP);
        if args.list && args.arch.is_none() && args.target.is_none() {
            let test_suite_dir = test_suite_dir(self.app.workspace_root(), test_group)?;
            let case_names = test_qemu::discover_all_qemu_cases(
                &test_suite_dir,
                args.test_case.as_deref(),
                "Axvisor",
                test_group,
            )?;
            println!("{}", test_qemu::render_case_tree(test_group, case_names));
            return Ok(());
        }

        let (arch, target) = parse_target(&args.arch, &args.target)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            test_group,
            &arch,
            &target,
            args.test_case.as_deref(),
        )?;
        if args.list {
            let case_names = cases.iter().map(|case| case.case.name.as_str());
            println!("{}", test_qemu::render_case_tree(test_group, case_names));
            return Ok(());
        }

        println!(
            "running axvisor qemu tests for arch: {} (target: {}, cases: {})",
            arch,
            target,
            cases.len()
        );

        let request = self.prepare_request(
            axvisor_qemu_test_build_args(&arch, None),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let request = Self::qemu_test_request(request);
        let cases = self
            .prepare_qemu_cases(&request, cases)
            .await
            .context("failed to load Axvisor qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut failed = Vec::new();
        let mut completed = 0;
        for group in Self::group_qemu_cases_by_build_config(&cases) {
            let (group_request, group_cargo) =
                Self::qemu_group_build_context(&request, group.build_config_path)?;
            rootfs::ensure_qemu_rootfs_ready(&group_request, self.app.workspace_root(), None)
                .await?;
            self.app
                .build(group_cargo.clone(), group_request.build_info_path.clone())
                .await
                .with_context(|| {
                    format!(
                        "failed to build Axvisor qemu test artifact for build group `{}` ({})",
                        group.build_group,
                        group.build_config_path.display()
                    )
                })?;

            for case in group.cases {
                completed += 1;
                let case_name = &case.case.case.name;
                println!("[{completed}/{total}] axvisor qemu {case_name}");

                let case_started = Instant::now();
                let result = self
                    .run_qemu_case(&group_request, &group_cargo, case)
                    .await
                    .with_context(|| format!("axvisor qemu test failed for case `{case_name}`"));
                match result {
                    Ok(()) => println!("ok: {} ({:.2?})", case_name, case_started.elapsed()),
                    Err(err) => {
                        eprintln!("failed: {}: {err:#}", case_name);
                        failed.push(case_name.clone());
                    }
                }
            }
        }

        println!("axvisor qemu total: {:.2?}", suite_started.elapsed());
        test_qemu::finalize_qemu_test_run("axvisor", "case", &failed)
    }

    pub(super) async fn test_uboot(&mut self, args: ArgsTestUboot) -> anyhow::Result<()> {
        let group = discover_uboot_test_group(self.app.workspace_root(), &args.board, &args.guest)?;
        let explicit_uboot_config = args.uboot_config.clone();
        let uboot_config_summary = explicit_uboot_config
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "using test-suit board config only".to_string());
        let board_test_config = group.board_test_config_path.clone();
        let board_test_config_summary = board_test_config.display().to_string();

        if let Some(path) = explicit_uboot_config.as_ref()
            && !path.exists()
        {
            bail!(
                "missing explicit U-Boot config `{}` for axvisor board tests",
                path.display()
            );
        }

        println!(
            "running axvisor uboot test for board: {} guest: {} case: {}",
            args.board, args.guest, group.name
        );

        let request = self.prepare_request(
            axvisor_board_test_build_args(&group),
            None,
            explicit_uboot_config.clone(),
            SnapshotPersistence::Discard,
        )?;

        let cargo = build::load_cargo_config(&request)?;
        let base_uboot = match request.uboot_config.as_deref() {
            Some(_) => self.load_uboot_config(&request, &cargo).await?,
            None => Some(
                self.app
                    .tool_mut()
                    .ensure_uboot_config_for_cargo(&cargo)
                    .await?,
            ),
        };
        let board_config = self
            .load_board_config(&cargo, Some(board_test_config.as_path()))
            .await?;
        let uboot = Some(merge_board_test_uboot_config(base_uboot, board_config));
        self.app
            .uboot(cargo, request.build_info_path, uboot)
            .await
            .with_context(|| {
                format!(
                    "axvisor uboot test failed for board `{}` guest `{}` case `{}` \
                     (build_config={}, board_test_config={}, uboot_config={})",
                    args.board,
                    args.guest,
                    group.name,
                    group.build_config.display(),
                    board_test_config_summary,
                    uboot_config_summary
                )
            })
    }

    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        if args.list && args.test_group.is_none() {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    match discover_board_test_groups(
                        self.app.workspace_root(),
                        &group,
                        args.test_case.as_deref(),
                        args.board.as_deref(),
                    ) {
                        Ok(groups) if groups.is_empty() => None,
                        Ok(groups) => Some(Ok((
                            group,
                            groups
                                .into_iter()
                                .map(|group| (group.name, group.board_name))
                                .collect::<Vec<_>>(),
                        ))),
                        Err(err) => {
                            let message = err.to_string();
                            if message.starts_with("no Axvisor ") {
                                None
                            } else {
                                Some(Err(err))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                bail!(
                    "no Axvisor board test groups found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!(
                "{}",
                test_qemu::render_labeled_case_forest("axvisor", groups)
            );
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(AXVISOR_NORMAL_GROUP);
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            test_group,
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = groups
                .into_iter()
                .map(|group| (group.name, group.board_name))
                .collect::<Vec<_>>();
            println!(
                "{}",
                test_qemu::render_labeled_case_forest("axvisor", [(test_group, case_names)])
            );
            return Ok(());
        }
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

            println!("[{}/{}] axvisor board {}", index + 1, total, group_label);

            let result = async {
                let request = self.prepare_request(
                    axvisor_board_test_build_args(&group),
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
                            "axvisor board test failed for group `{}` (build_config={}, \
                             board_test_config={})",
                            group_label,
                            group.build_config.display(),
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

        board_test::finalize_board_test_run("axvisor", &failed)
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cases: Vec<AxvisorQemuCase>,
    ) -> anyhow::Result<Vec<PreparedAxvisorQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        for case in cases {
            let mut request = request.clone();
            request.build_info_path = case.build_config_path.clone();
            let cargo = build::load_cargo_config(&request)?;
            let qemu = self
                .app
                .tool_mut()
                .read_qemu_config_from_path_for_cargo(&cargo, &case.case.qemu_config_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read Axvisor qemu config for case `{}`",
                        case.case.display_name
                    )
                })?;
            test_qemu::validate_grouped_qemu_commands(&qemu, &case.case, "Axvisor")?;
            prepared.push(PreparedAxvisorQemuCase { case, qemu });
        }

        Ok(prepared)
    }

    fn group_qemu_cases_by_build_config(
        cases: &[PreparedAxvisorQemuCase],
    ) -> Vec<test_qemu::QemuCaseGroup<'_, PreparedAxvisorQemuCase>> {
        test_qemu::group_cases_by_build_config(cases)
    }

    fn qemu_group_build_context(
        request: &ResolvedAxvisorRequest,
        build_config_path: &Path,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, Cargo)> {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        let cargo = build::load_cargo_config(&request)?;
        request.vmconfigs = qemu_group_vmconfigs(&request, &cargo)?;

        Ok((request, cargo))
    }

    fn qemu_test_request(mut request: ResolvedAxvisorRequest) -> ResolvedAxvisorRequest {
        request.smp = None;
        request
    }

    async fn load_qemu_case_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        case: &PreparedAxvisorQemuCase,
    ) -> anyhow::Result<(QemuConfig, test_case::PreparedCaseAssets)> {
        let mut qemu = case.qemu.clone();
        let asset_config = axvisor_case_asset_config();
        test_case::apply_grouped_qemu_config(
            &mut qemu,
            &case.case.case,
            &asset_config.grouped_runner,
        );
        test_qemu::apply_timeout_scale(&mut qemu);

        let rootfs_path = rootfs::qemu_rootfs_path(request, self.app.workspace_root(), None)?;
        let prepared_assets = test_case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            &case.case.case,
            rootfs_path,
            asset_config,
        )
        .await?;
        rootfs::patch_qemu_rootfs_path(&mut qemu, &prepared_assets.rootfs_path);
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        Ok((qemu, prepared_assets))
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        case: &PreparedAxvisorQemuCase,
    ) -> anyhow::Result<()> {
        let prepare_started = Instant::now();
        let (qemu, prepared_assets) = self.load_qemu_case_config(request, case).await?;
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
        println!(
            "  qemu config: {} (timeout={})",
            case.case.case.qemu_config_path.display(),
            test_qemu::qemu_timeout_summary(&qemu)
        );
        println!("  rootfs: {}", prepared_assets.rootfs_path.display());
        let qemu_started = Instant::now();
        let result = self.app.run_qemu(cargo, qemu).await;
        println!("  qemu run: {:.2?}", qemu_started.elapsed());
        // Remove the per-case rootfs copy immediately after the run so disk
        // usage stays bounded to ~1 active copy at a time rather than
        // accumulating one copy per case.
        test_case::remove_case_rootfs_copy(prepared_assets.rootfs_copy_to_remove.as_deref());
        test_case::remove_case_run_dir(prepared_assets.run_dir_to_remove.as_deref());
        result
    }
}

fn qemu_group_vmconfigs(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
) -> anyhow::Result<Vec<PathBuf>> {
    let Some(value) = cargo.env.get("AXVISOR_VM_CONFIGS") else {
        return Ok(Vec::new());
    };
    std::env::split_paths(value)
        .map(|path| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(request
                    .axvisor_dir
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(&request.axvisor_dir)
                    .join(path))
            }
        })
        .collect()
}

fn axvisor_qemu_test_build_args(arch: &str, config: Option<PathBuf>) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config,
        arch: Some(arch.to_string()),
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}

fn axvisor_case_asset_config() -> test_case::CaseAssetConfig {
    test_case::CaseAssetConfig {
        grouped_runner: test_case::GroupedCaseRunnerConfig {
            runner_name: "axvisor-run-case-tests".to_string(),
            runner_path: "/usr/bin/axvisor-run-case-tests".to_string(),
            begin_marker: "AXVISOR_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "AXVISOR_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "AXVISOR_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "AXVISOR_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "AXVISOR_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^AXVISOR_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^AXVISOR_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: test_case::CaseScriptEnvConfig {
            staging_root: "AXVISOR_TEST_STAGING_ROOT".to_string(),
            case_dir: "AXVISOR_TEST_CASE_DIR".to_string(),
            case_c_dir: "AXVISOR_TEST_CASE_C_DIR".to_string(),
            case_work_dir: "AXVISOR_TEST_CASE_WORK_DIR".to_string(),
            case_build_dir: "AXVISOR_TEST_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "AXVISOR_TEST_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}

fn axvisor_board_test_build_args(group: &BoardTestGroup) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(group.build_config.clone()),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_qemu_config(root: &Path, case: &str, arch: &str, body: &str) -> PathBuf {
        write_qemu_config_in_group(root, "normal", "default", case, arch, body)
    }

    fn write_qemu_config_in_group(
        root: &Path,
        group: &str,
        build_group: &str,
        case: &str,
        arch: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axvisor")
            .join(group)
            .join(build_group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("qemu-{arch}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_qemu_build_config(
        root: &Path,
        group: &str,
        build_group: &str,
        target: &str,
    ) -> PathBuf {
        let dir = root.join("test-suit/axvisor").join(group).join(build_group);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("build-{target}.toml"));
        fs::write(
            &path,
            format!("target = \"{target}\"\nfeatures = []\nlog = \"Info\"\nvm_configs = []\n"),
        )
        .unwrap();
        path
    }

    fn write_board_build_config(root: &Path, build_group: &str) -> PathBuf {
        write_qemu_build_config(
            root,
            "normal",
            build_group,
            "aarch64-unknown-none-softfloat",
        )
    }

    fn write_board_config(root: &Path, case: &str, name: &str, body: &str) -> PathBuf {
        write_board_config_in_group(root, "normal", "default", case, name, body)
    }

    fn write_board_config_in_group(
        root: &Path,
        group: &str,
        build_group: &str,
        case: &str,
        name: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axvisor")
            .join(group)
            .join(build_group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("board-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn axvisor_request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: PathBuf::from("/tmp/os/axvisor"),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: Vec::new(),
        }
    }

    #[test]
    fn parses_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("loongarch64".to_string()), &None).unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("riscv64".to_string()), &None).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
    }

    #[test]
    fn accepts_full_target_triples() {
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&None, &Some("riscv64gc-unknown-none-elf".to_string())).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
        assert_eq!(
            parse_target(
                &None,
                &Some("loongarch64-unknown-none-softfloat".to_string())
            )
            .unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_arches() {
        let err = parse_target(&Some("mips64".to_string()), &None).unwrap_err();
        let err = err.to_string();

        assert!(err.contains("mips64"));
        assert!(err.contains("aarch64"));
        assert!(err.contains("loongarch64"));
        assert!(err.contains("riscv64"));
        assert!(err.contains("x86_64"));
    }

    #[test]
    fn qemu_test_request_ignores_inherited_smp() {
        let mut request = axvisor_request(
            PathBuf::from("/tmp/build-riscv64gc-unknown-none-elf.toml"),
            "riscv64",
            "riscv64gc-unknown-none-elf",
        );
        request.smp = Some(1);

        let request = Axvisor::qemu_test_request(request);

        assert_eq!(request.smp, None);
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        let build_config = write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );
        write_qemu_config(
            root.path(),
            "x86-only",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke"]
        );
        assert_eq!(cases[0].build_config_path, build_config);
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            Some("smoke"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("none provide `qemu-aarch64.toml`"));
    }

    #[test]
    fn selected_qemu_case_skips_non_qemu_case_with_same_name() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "board-orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(
            root.path(),
            "normal",
            "qemu",
            "aarch64-unknown-none-softfloat",
        );
        write_board_config_in_group(
            root.path(),
            "normal",
            "board-orangepi-5-plus",
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
        );
        write_qemu_config_in_group(
            root.path(),
            "normal",
            "qemu",
            "smoke",
            "aarch64",
            "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            Some("smoke"),
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].build_group, "qemu");
        assert_eq!(cases[0].case.name, "smoke");
    }

    #[test]
    fn discovers_qemu_cases_from_selected_group() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(
            root.path(),
            "stress",
            "stress-default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );
        write_qemu_config_in_group(
            root.path(),
            "stress",
            "stress-default",
            "load",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"stress\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "stress",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["load"]
        );
    }

    #[test]
    fn rejects_unknown_qemu_test_group() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "unknown",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported Axvisor test group `unknown`")
        );
        assert!(err.to_string().contains("normal"));
    }

    #[test]
    fn returns_all_board_test_groups_when_no_filter_is_given() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
        );

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus-linux", "smoke/phytiumpi-linux"]
        );
    }

    #[test]
    fn discovers_board_case_when_case_dir_contains_build_config() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/axvisor/normal/smoke");
        fs::create_dir_all(&case_dir).unwrap();
        let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
        fs::write(
            &build_config,
            "target = \"aarch64-unknown-none-softfloat\"\n",
        )
        .unwrap();
        let board_test_config = case_dir.join("board-phytiumpi-linux.toml");
        fs::write(&board_test_config, "board_type = \"PhytiumPi\"\n").unwrap();

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "phytiumpi-linux");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn board_case_uses_unique_nearest_build_config_without_target_assumption() {
        let root = tempdir().unwrap();
        let wrapper_dir = root.path().join("test-suit/axvisor/normal/board-custom");
        let case_dir = wrapper_dir.join("smoke");
        fs::create_dir_all(&case_dir).unwrap();
        let build_config = wrapper_dir.join("build-riscv64gc-unknown-none-elf.toml");
        fs::write(&build_config, "target = \"riscv64gc-unknown-none-elf\"\n").unwrap();
        let board_test_config = case_dir.join("board-custom.toml");
        fs::write(&board_test_config, "board_type = \"Custom\"\n").unwrap();

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "custom");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_group_by_case() {
        let root = tempdir().unwrap();
        let build_config = write_board_build_config(root.path(), "default");
        let board_test_config = write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );

        let groups =
            discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "phytiumpi-linux");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_groups_by_board() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "syscall",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
        );

        let groups =
            discover_board_test_groups(root.path(), "normal", None, Some("phytiumpi-linux"))
                .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/phytiumpi-linux", "syscall/phytiumpi-linux"]
        );
    }

    #[test]
    fn discovers_uboot_test_group_from_board_cases() {
        let root = tempdir().unwrap();
        let build_config = write_board_build_config(root.path(), "board-rdk-s100");
        let board_test_config = write_board_config_in_group(
            root.path(),
            "normal",
            "board-rdk-s100",
            "smoke",
            "rdk-s100-linux",
            "board_type = \"RDK-S100\"\nuboot_cmd = [\"run ab_select_cmd\", \"run \
             avb_boot\"]\nsuccess_regex = [\"ubuntu login:\"]\nfail_regex = [\"(?i)panic\"]\n",
        );

        let group = discover_uboot_test_group(root.path(), "rdk-s100", "linux").unwrap();

        assert_eq!(group.name, "smoke");
        assert_eq!(group.board_name, "rdk-s100-linux");
        assert_eq!(group.build_config, build_config);
        assert_eq!(group.board_test_config_path, board_test_config);
    }

    #[test]
    fn uboot_test_config_uses_board_case_matchers_and_keeps_base_local_config() {
        let base = UbootConfig {
            dtb_file: Some("${env:BOARD_DTB}".to_string()),
            success_regex: vec!["old-ok".to_string()],
            fail_regex: vec!["old-fail".to_string()],
            uboot_cmd: Some(vec!["old-boot".to_string()]),
            shell_prefix: Some("old-login:".to_string()),
            timeout: Some(300),
            local: ostool::run::uboot::LocalUbootConfig {
                serial: Some("/dev/ttyUSB1".to_string()),
                baud_rate: Some("1500000".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let board_test = ostool::board::config::BoardRunConfig {
            board_type: "RDK-S100".to_string(),
            success_regex: vec!["ubuntu login:".to_string()],
            fail_regex: vec!["(?i)panic".to_string()],
            uboot_cmd: Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string(),
            ]),
            shell_prefix: Some("ubuntu login:".to_string()),
            ..Default::default()
        };

        let merged = merge_board_test_uboot_config(Some(base), board_test);

        assert_eq!(merged.success_regex, vec!["ubuntu login:"]);
        assert_eq!(merged.fail_regex, vec!["(?i)panic"]);
        assert_eq!(
            merged.uboot_cmd,
            Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string()
            ])
        );
        assert_eq!(merged.shell_prefix.as_deref(), Some("ubuntu login:"));
        assert_eq!(merged.dtb_file.as_deref(), Some("${env:BOARD_DTB}"));
        assert_eq!(merged.timeout, Some(300));
        assert_eq!(merged.local.serial.as_deref(), Some("/dev/ttyUSB1"));
        assert_eq!(merged.local.baud_rate.as_deref(), Some("1500000"));
    }

    #[test]
    fn ignores_qemu_only_build_groups_when_discovering_board_tests() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "qemu",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "qemu", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
        );

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "orangepi-5-plus-linux");
    }

    #[test]
    fn rejects_unknown_board_test_board() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );

        let err =
            discover_board_test_groups(root.path(), "normal", None, Some("unknown")).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor board test board `unknown`")
        );
        assert!(err.to_string().contains("phytiumpi-linux"));
    }

    #[test]
    fn rejects_unknown_board_test_case() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );

        let err =
            discover_board_test_groups(root.path(), "normal", Some("unknown"), None).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor board test case `unknown`")
        );
        assert!(err.to_string().contains("smoke"));
    }

    #[test]
    fn rejects_empty_board_test_group() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("test-suit/axvisor/empty")).unwrap();

        let err = discover_board_test_groups(root.path(), "empty", None, None).unwrap_err();

        assert!(
            err.to_string()
                .contains("no Axvisor board test groups found under")
        );
    }

    #[test]
    fn board_case_config_is_also_valid_board_run_config() {
        let config: ostool::board::config::BoardRunConfig = toml::from_str(
            "board_type = \"PhytiumPi\"\nshell_prefix = \"login:\"\nshell_init_cmd = \
             \"root\"\nsuccess_regex = [\"(?m)^root@.*#\\\\s*$\"]\n",
        )
        .unwrap();

        assert_eq!(config.board_type, "PhytiumPi");
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
    }
}
