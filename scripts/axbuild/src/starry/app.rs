use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use serde::Deserialize;

use super::{board, rootfs};
use crate::{
    context::starry_target_for_arch_checked,
    rootfs::inject,
    support::process::ProcessExt,
    test::{case::TestQemuCase, qemu as qemu_test},
};

#[derive(Args, Debug, Clone)]
pub struct ArgsApp {
    #[command(subcommand)]
    pub command: AppCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum AppCommand {
    /// List discovered StarryOS apps
    List(ArgsAppList),
    /// Build and run a StarryOS QEMU app
    Qemu(ArgsAppQemu),
    /// Build and run a StarryOS app on a remote board
    Board(ArgsAppBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppList {
    #[arg(long, value_enum)]
    pub kind: Option<StarryAppKind>,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppQemu {
    /// Run all discovered QEMU apps after capability filtering
    #[arg(long)]
    pub all: bool,

    /// Select apps/starry/<CASE>
    #[arg(short = 't', long = "test-case", value_name = "CASE")]
    pub test_case: Option<String>,

    /// Declare an available capability, e.g. board:OrangePi-5-Plus
    #[arg(long = "cap", value_name = "CAP")]
    pub caps: Vec<String>,

    #[arg(long)]
    pub arch: Option<String>,

    #[arg(long = "qemu-config")]
    pub qemu_config: Option<PathBuf>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsAppBoard {
    /// Select apps/starry/<CASE>
    #[arg(short = 't', long = "test-case", value_name = "CASE")]
    pub test_case: String,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StarryAppKind {
    Qemu,
    Board,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppCase {
    pub(crate) name: String,
    pub(crate) kind: StarryAppKind,
    pub(crate) case_dir: PathBuf,
    pub(crate) prebuild_path: Option<PathBuf>,
    pub(crate) requires: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppBoardCase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) init_path: PathBuf,
    pub(crate) init_cmd: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_config_path: PathBuf,
    pub(crate) target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryAppQemuCase {
    pub(crate) name: String,
    pub(crate) arch: String,
    pub(crate) target: String,
    pub(crate) build_config_path: Option<PathBuf>,
    pub(crate) qemu_config_path: Option<PathBuf>,
    pub(crate) rootfs_path: PathBuf,
    pub(crate) test_commands: Vec<String>,
    pub(crate) host_symbolize_success_regex: Vec<String>,
    pub(crate) subcases: Vec<crate::test::case::TestQemuSubcase>,
}

#[derive(Debug)]
struct LoadedQemuAppCaseFields {
    test_case: TestQemuCase,
    rootfs_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildConfigCandidate {
    path: PathBuf,
    target: String,
}

#[derive(Debug, Deserialize)]
struct BuildConfigTarget {
    target: Option<String>,
}

pub(crate) fn resolve_board_case(
    workspace_root: &Path,
    case_name: &str,
    explicit_board_config: Option<&Path>,
) -> anyhow::Result<StarryAppBoardCase> {
    let case_name = validate_case_name(case_name)?;
    let apps_dir = apps_starry_dir(workspace_root);
    ensure!(
        apps_dir.is_dir(),
        "missing Starry apps directory `{}`",
        apps_dir.display()
    );

    let case_dir = apps_dir.join(case_name);
    if !case_dir.is_dir() {
        bail!(
            "unknown Starry app case `{case_name}` in {}; available cases: {}",
            apps_dir.display(),
            available_case_names(&apps_dir)?
        );
    }

    let init_path = case_dir.join("init.sh");
    ensure!(
        init_path.is_file(),
        "Starry app case `{case_name}` is missing `{}`",
        init_path.display()
    );
    let init_cmd = fs::read_to_string(&init_path)
        .with_context(|| format!("failed to read {}", init_path.display()))?;
    let init_cmd = init_cmd.trim().to_string();
    ensure!(
        !init_cmd.is_empty(),
        "Starry app case `{case_name}` has an empty init script `{}`",
        init_path.display()
    );

    let board_config_path = match explicit_board_config {
        Some(path) => resolve_explicit_board_config(&case_dir, path),
        None => discover_case_board_config(&case_dir)?,
    };
    let default_target = default_target_for_board_config(workspace_root, &board_config_path)?;
    let (build_config_path, target) =
        discover_case_build_config(&case_dir, default_target.as_deref())?;

    Ok(StarryAppBoardCase {
        name: case_name.to_string(),
        case_dir,
        init_path,
        init_cmd,
        build_config_path,
        board_config_path,
        target,
    })
}

pub(crate) fn discover_apps(workspace_root: &Path) -> anyhow::Result<Vec<StarryAppCase>> {
    discover_apps_with_ignore(workspace_root, true)
}

fn discover_apps_with_ignore(
    workspace_root: &Path,
    respect_ignore: bool,
) -> anyhow::Result<Vec<StarryAppCase>> {
    let apps_dir = apps_starry_dir(workspace_root);
    ensure!(
        apps_dir.is_dir(),
        "missing Starry apps directory `{}`",
        apps_dir.display()
    );

    let ignored = if respect_ignore {
        ignored_app_names(workspace_root)?
    } else {
        BTreeSet::new()
    };
    let mut apps = Vec::new();
    collect_apps_in_dir(&apps_dir, &apps_dir, &ignored, &mut apps)?;
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

fn collect_apps_in_dir(
    apps_dir: &Path,
    dir: &Path,
    ignored: &BTreeSet<String>,
    apps: &mut Vec<StarryAppCase>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }
        let name = relative_app_name(apps_dir, &case_dir)?;
        if is_ignored_app(ignored, &name) {
            continue;
        }
        if let Some(kind) = infer_app_kind(&case_dir)? {
            apps.push(StarryAppCase {
                name,
                kind,
                prebuild_path: optional_file(case_dir.join("prebuild.sh")),
                requires: read_requires(&case_dir)?,
                case_dir,
            });
            continue;
        }
        collect_apps_in_dir(apps_dir, &case_dir, ignored, apps)?;
    }
    Ok(())
}

fn relative_app_name(apps_dir: &Path, case_dir: &Path) -> anyhow::Result<String> {
    let relative = case_dir.strip_prefix(apps_dir).with_context(|| {
        format!(
            "failed to make {} relative to {}",
            case_dir.display(),
            apps_dir.display()
        )
    })?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

pub(crate) fn print_apps(workspace_root: &Path, kind: Option<StarryAppKind>) -> anyhow::Result<()> {
    for app in filtered_apps(workspace_root, kind)? {
        let kind = match app.kind {
            StarryAppKind::Qemu => "qemu",
            StarryAppKind::Board => "board",
        };
        let prebuild = if app.prebuild_path.is_some() {
            " prebuild"
        } else {
            ""
        };
        println!("{kind}	{}{prebuild}", app.name);
    }
    Ok(())
}

pub(crate) fn selected_apps(
    workspace_root: &Path,
    args: &ArgsAppQemu,
    kind: StarryAppKind,
) -> anyhow::Result<Vec<StarryAppCase>> {
    ensure!(
        args.all ^ args.test_case.is_some(),
        "`starry app qemu` requires exactly one of --all or -t/--test-case"
    );

    let mut apps = if args.test_case.is_some() {
        discover_apps_with_ignore(workspace_root, false)?
    } else {
        discover_apps(workspace_root)?
    };
    apps.retain(|app| app.kind == kind);
    if args.all && args.qemu_config.is_none() {
        let arch = args.arch.as_deref().unwrap_or("x86_64");
        apps.retain(|app| app.kind != StarryAppKind::Qemu || qemu_app_supports_arch(app, arch));
    }
    if let Some(case_name) = args.test_case.as_deref() {
        let case_name = validate_case_name(case_name)?;
        apps.retain(|app| app.name == case_name);
        ensure!(
            !apps.is_empty(),
            "unknown or ignored Starry app case `{case_name}`"
        );
    }
    Ok(apps)
}

pub(crate) fn missing_caps(app: &StarryAppCase, caps: &[String]) -> Vec<String> {
    let caps = caps.iter().map(String::as_str).collect::<BTreeSet<_>>();
    app.requires
        .iter()
        .filter(|required| !caps.contains(required.as_str()))
        .cloned()
        .collect()
}

pub(crate) async fn prepare_qemu_app_case(
    workspace_root: &Path,
    app: &StarryAppCase,
    arch: Option<&str>,
    explicit_qemu_config: Option<&Path>,
) -> anyhow::Result<StarryAppQemuCase> {
    ensure!(
        app.kind == StarryAppKind::Qemu,
        "Starry app `{}` is not a QEMU app",
        app.name
    );
    let qemu_config_path = resolve_qemu_config(app, arch, explicit_qemu_config)?;
    let arch = arch
        .map(str::to_string)
        .or_else(|| {
            qemu_config_path
                .as_deref()
                .and_then(arch_from_qemu_config_path)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "x86_64".to_string());
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let build_config_path = discover_optional_build_config(&app.case_dir, &target)?;
    let fields = qemu_config_path
        .as_deref()
        .map(|path| load_qemu_app_case_fields(workspace_root, app, path))
        .transpose()?;
    let rootfs_path = prepare_qemu_app_rootfs(
        workspace_root,
        app,
        &arch,
        &target,
        fields
            .as_ref()
            .and_then(|fields| fields.rootfs_path.as_deref()),
    )
    .await?;

    Ok(StarryAppQemuCase {
        name: app.name.clone(),
        arch,
        target,
        build_config_path,
        qemu_config_path,
        rootfs_path,
        test_commands: fields
            .as_ref()
            .map(|fields| fields.test_case.test_commands.clone())
            .unwrap_or_default(),
        host_symbolize_success_regex: fields
            .as_ref()
            .map(|fields| fields.test_case.host_symbolize_success_regex.clone())
            .unwrap_or_default(),
        subcases: fields
            .map(|fields| fields.test_case.subcases)
            .unwrap_or_default(),
    })
}

pub(crate) fn app_qemu_test_case(
    case: &StarryAppQemuCase,
    case_dir: PathBuf,
) -> Option<TestQemuCase> {
    let qemu_config_path = case.qemu_config_path.clone()?;
    Some(TestQemuCase {
        name: case.name.clone(),
        display_name: case.name.clone(),
        case_dir,
        qemu_config_path,
        test_commands: case.test_commands.clone(),
        host_symbolize_success_regex: case.host_symbolize_success_regex.clone(),
        host_http_server: None,
        subcases: case.subcases.clone(),
        grouped_subcase_filter: None,
    })
}

fn load_qemu_app_case_fields(
    workspace_root: &Path,
    app: &StarryAppCase,
    qemu_config_path: &Path,
) -> anyhow::Result<LoadedQemuAppCaseFields> {
    let test_case = qemu_test::load_test_qemu_case_fields(
        app.name.clone(),
        app.name.clone(),
        app.case_dir.clone(),
        qemu_config_path.to_path_buf(),
        "Starry app",
        true,
    )?;
    let rootfs_path = qemu_app_config_rootfs_path(workspace_root, qemu_config_path)?;

    Ok(LoadedQemuAppCaseFields {
        test_case,
        rootfs_path,
    })
}

fn qemu_app_config_rootfs_path(
    workspace_root: &Path,
    qemu_config_path: &Path,
) -> anyhow::Result<Option<PathBuf>> {
    let qemu = read_qemu_app_config(qemu_config_path)?;
    Ok(qemu_app_managed_rootfs_paths(workspace_root, &qemu)?
        .into_iter()
        .next())
}

fn read_qemu_app_config(qemu_config_path: &Path) -> anyhow::Result<ostool::run::qemu::QemuConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))
}

fn qemu_app_managed_rootfs_paths(
    workspace_root: &Path,
    qemu: &ostool::run::qemu::QemuConfig,
) -> anyhow::Result<Vec<PathBuf>> {
    crate::rootfs::qemu::drive_file_paths(qemu)
        .into_iter()
        .filter_map(|path| {
            crate::image::storage::resolve_managed_rootfs_path(workspace_root, &path).transpose()
        })
        .collect()
}

fn optional_file(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

fn filtered_apps(
    workspace_root: &Path,
    kind: Option<StarryAppKind>,
) -> anyhow::Result<Vec<StarryAppCase>> {
    let mut apps = discover_apps(workspace_root)?;
    if let Some(kind) = kind {
        apps.retain(|app| app.kind == kind);
    }
    Ok(apps)
}

fn ignored_app_names(workspace_root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let path = workspace_root.join("apps/.ignore");
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_matches('/').to_string())
        .collect())
}

fn is_ignored_app(ignored: &BTreeSet<String>, name: &str) -> bool {
    ignored.contains(name)
        || ignored.contains(&format!("starry/{name}"))
        || ignored.contains(&format!("apps/starry/{name}"))
}

fn infer_app_kind(case_dir: &Path) -> anyhow::Result<Option<StarryAppKind>> {
    let has_qemu = !collect_prefixed_toml_files(case_dir, "qemu-")?.is_empty();
    let has_board = case_dir.join("init.sh").is_file()
        && !collect_prefixed_toml_files(case_dir, "board-")?.is_empty();
    let has_prebuild = case_dir.join("prebuild.sh").is_file();

    match (has_qemu, has_board, has_prebuild) {
        (true, false, _) => Ok(Some(StarryAppKind::Qemu)),
        (false, true, _) => Ok(Some(StarryAppKind::Board)),
        (false, false, true) => Ok(Some(StarryAppKind::Qemu)),
        (false, false, false) => Ok(None),
        (true, true, _) => bail!(
            "Starry app `{}` has both qemu-* and board-* configs; split it or make kind explicit",
            case_dir.display()
        ),
    }
}

fn read_requires(case_dir: &Path) -> anyhow::Result<Vec<String>> {
    let path = case_dir.join("requires");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

fn resolve_qemu_config(
    app: &StarryAppCase,
    arch: Option<&str>,
    explicit_qemu_config: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(path) = explicit_qemu_config {
        return Ok(Some(resolve_case_relative_path(&app.case_dir, path)));
    }

    let arch = arch.unwrap_or("x86_64");
    let path = app.case_dir.join(qemu_config_name(arch));
    if path.is_file() {
        return Ok(Some(path));
    }

    let variants = qemu_config_variants_for_arch(&app.case_dir, arch)?;
    if !variants.is_empty() {
        bail!(
            "Starry app `{}` does not provide `{}`; pass --qemu-config to select one of: {}",
            app.name,
            qemu_config_name(arch),
            format_paths(&variants)
        );
    }

    let configs = collect_prefixed_toml_files(&app.case_dir, "qemu-")?;
    if !configs.is_empty() {
        bail!(
            "Starry app `{}` does not provide `{}`; available QEMU configs: {}",
            app.name,
            qemu_config_name(arch),
            format_paths(&configs)
        );
    }
    Ok(None)
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn qemu_app_supports_arch(app: &StarryAppCase, arch: &str) -> bool {
    app.case_dir.join(qemu_config_name(arch)).is_file()
        || !qemu_config_variants_for_arch(&app.case_dir, arch)
            .unwrap_or_default()
            .is_empty()
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

fn qemu_config_variants_for_arch(case_dir: &Path, arch: &str) -> anyhow::Result<Vec<PathBuf>> {
    let prefix = format!("qemu-{arch}-");
    collect_prefixed_toml_files(case_dir, &prefix)
}

fn arch_from_qemu_config_path(path: &Path) -> Option<&str> {
    let stem = path.file_stem()?.to_str()?;
    let rest = stem.strip_prefix("qemu-")?;
    rest.split('-').next().filter(|arch| !arch.is_empty())
}

fn discover_optional_build_config(
    case_dir: &Path,
    target: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let mut dir = Some(case_dir);
    while let Some(current_dir) = dir {
        if let Some(path) = resolve_exact_build_config_path(current_dir, target)? {
            return Ok(Some(path));
        }
        dir = current_dir.parent();
    }
    Ok(None)
}

fn resolve_exact_build_config_path(dir: &Path, target: &str) -> anyhow::Result<Option<PathBuf>> {
    let path = dir.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(Some(path));
    }

    let legacy_candidates = legacy_build_config_candidates(dir, target);
    if !legacy_candidates.is_empty() {
        bail!(
            "unsupported legacy build config name(s) under {}: {}; expected only              \
             `build-{target}.toml`",
            dir.display(),
            legacy_candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(None)
}

fn legacy_build_config_candidates(dir: &Path, target: &str) -> Vec<PathBuf> {
    let Some(arch) = arch_from_target_name(target) else {
        return Vec::new();
    };
    [
        dir.join(format!(".build-{target}.toml")),
        dir.join(format!("build-{arch}.toml")),
        dir.join(format!(".build-{arch}.toml")),
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .collect()
}

fn arch_from_target_name(target: &str) -> Option<&str> {
    target.split_once('-').map(|(arch, _)| arch)
}

async fn prepare_qemu_app_rootfs(
    workspace_root: &Path,
    app: &StarryAppCase,
    arch: &str,
    target: &str,
    configured_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let rootfs_path = match configured_rootfs {
        Some(path) => path.to_path_buf(),
        None => crate::image::storage::default_rootfs_path(workspace_root, arch)?,
    };
    if app.prebuild_path.is_none() {
        if let Some(configured) = configured_rootfs {
            crate::image::storage::ensure_optional_managed_rootfs(
                workspace_root,
                arch,
                Some(configured),
            )
            .await?;
            rootfs::ensure_apk_region_in_rootfs(configured)?;
            return Ok(configured.to_path_buf());
        }
        return rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await;
    }

    let default_rootfs = rootfs::ensure_rootfs_in_tmp_dir(workspace_root, arch, target).await?;
    if let Some(parent) = rootfs_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if !rootfs_path.exists() {
        fs::copy(&default_rootfs, &rootfs_path).with_context(|| {
            format!(
                "failed to copy default rootfs {} to {}",
                default_rootfs.display(),
                rootfs_path.display()
            )
        })?;
    }

    let layout_root = workspace_root
        .join("tmp/axbuild/starry-app")
        .join(&app.name);
    let staging_root = layout_root.join("staging-root");
    let overlay_dir = layout_root.join("overlay");

    let prepare_result = (|| -> anyhow::Result<()> {
        reset_dir(&staging_root)?;
        reset_dir(&overlay_dir)?;

        if let Some(prebuild_path) = app.prebuild_path.as_deref() {
            let mut command = Command::new("bash");
            command
                .arg(prebuild_path)
                .current_dir(&app.case_dir)
                .env("STARRY_APP_NAME", &app.name)
                .env("STARRY_APP_DIR", &app.case_dir)
                .env("STARRY_WORKSPACE", workspace_root)
                .env("STARRY_ARCH", arch)
                .env("STARRY_ROOTFS", &rootfs_path)
                .env("STARRY_STAGING_ROOT", &staging_root)
                .env("STARRY_OVERLAY_DIR", &overlay_dir);
            command
                .exec()
                .with_context(|| format!("failed to run {}", prebuild_path.display()))?;
        }

        inject::inject_overlay(&rootfs_path, &overlay_dir)
    })();
    prepare_result?;
    Ok(rootfs_path)
}

fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn apps_starry_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("apps/starry")
}

fn validate_case_name(case_name: &str) -> anyhow::Result<&str> {
    let case_name = case_name.trim();
    ensure!(!case_name.is_empty(), "Starry app case name is empty");
    let path = Path::new(case_name);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
        "invalid Starry app case name `{case_name}`"
    );
    Ok(case_name)
}

fn available_case_names(apps_dir: &Path) -> anyhow::Result<String> {
    let mut cases = Vec::new();
    for entry in
        fs::read_dir(apps_dir).with_context(|| format!("failed to read {}", apps_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        cases.push(name);
    }
    cases.sort();
    if cases.is_empty() {
        Ok("<none>".to_string())
    } else {
        Ok(cases.join(", "))
    }
}

fn discover_case_board_config(case_dir: &Path) -> anyhow::Result<PathBuf> {
    let mut configs = collect_prefixed_toml_files(case_dir, "board-")?;
    match configs.len() {
        0 => bail!(
            "Starry app case `{}` does not provide a board-<board>.toml config",
            case_dir.display()
        ),
        1 => Ok(configs.remove(0)),
        _ => bail!(
            "Starry app case `{}` provides multiple board configs; pass --board-config",
            case_dir.display()
        ),
    }
}

fn resolve_explicit_board_config(case_dir: &Path, path: &Path) -> PathBuf {
    resolve_case_relative_path(case_dir, path)
}

fn resolve_case_relative_path(case_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    let case_relative = case_dir.join(path);
    if case_relative.exists() {
        case_relative
    } else {
        path.to_path_buf()
    }
}

fn discover_case_build_config(
    case_dir: &Path,
    preferred_target: Option<&str>,
) -> anyhow::Result<(PathBuf, String)> {
    let mut candidates = collect_build_config_candidates(case_dir)?;
    ensure!(
        !candidates.is_empty(),
        "Starry app case `{}` does not provide a build-<target>.toml config",
        case_dir.display()
    );

    if let Some(preferred_target) = preferred_target
        && let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.target == preferred_target)
    {
        let candidate = candidates.remove(index);
        return Ok((candidate.path, candidate.target));
    }

    match candidates.len() {
        1 => {
            let candidate = candidates.remove(0);
            Ok((candidate.path, candidate.target))
        }
        _ => bail!(
            "Starry app case `{}` provides multiple build configs; pass a board config that maps \
             to one target or keep one build config",
            case_dir.display()
        ),
    }
}

fn collect_prefixed_toml_files(case_dir: &Path, prefix: &str) -> anyhow::Result<Vec<PathBuf>> {
    let mut configs = Vec::new();
    for entry in
        fs::read_dir(case_dir).with_context(|| format!("failed to read {}", case_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.starts_with(prefix) {
            configs.push(path);
        }
    }
    configs.sort();
    Ok(configs)
}

fn collect_build_config_candidates(case_dir: &Path) -> anyhow::Result<Vec<BuildConfigCandidate>> {
    let mut paths = collect_prefixed_toml_files(case_dir, "build-")?;
    paths.extend(collect_prefixed_toml_files(case_dir, ".build-")?);
    paths.sort();
    paths.dedup();

    paths
        .into_iter()
        .map(|path| {
            let target = build_config_target(&path)?;
            Ok(BuildConfigCandidate { path, target })
        })
        .collect()
}

fn build_config_target(path: &Path) -> anyhow::Result<String> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: BuildConfigTarget =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let filename_target = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(build_config_target_from_stem);

    if let (Some(parsed), Some(filename)) = (parsed.target.as_deref(), filename_target.as_deref())
        && parsed != filename
    {
        bail!(
            "build config `{}` target `{parsed}` does not match filename target `{filename}`",
            path.display()
        );
    }

    parsed.target.or(filename_target).ok_or_else(|| {
        anyhow::anyhow!(
            "build config `{}` must define top-level `target` or use build-<target>.toml",
            path.display()
        )
    })
}

fn build_config_target_from_stem(stem: &str) -> Option<String> {
    stem.strip_prefix("build-")
        .or_else(|| stem.strip_prefix(".build-"))
        .map(str::to_string)
        .filter(|target| !target.is_empty())
}

fn default_target_for_board_config(
    workspace_root: &Path,
    board_config_path: &Path,
) -> anyhow::Result<Option<String>> {
    let Some(stem) = board_config_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let Some(board_name) = stem.strip_prefix("board-") else {
        return Ok(None);
    };
    let build_config_path = workspace_root
        .join("os/StarryOS/configs/board")
        .join(format!("{board_name}.toml"));
    if !build_config_path.is_file() {
        return Ok(None);
    }
    Ok(Some(board::load_board_file(&build_config_path)?.target))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_case_file(root: &Path, case_name: &str, name: &str, body: &str) -> PathBuf {
        let path = root.join("apps/starry").join(case_name).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    fn write_board_default(root: &Path, board_name: &str, target: &str) -> PathBuf {
        let path = root
            .join("os/StarryOS/configs/board")
            .join(format!("{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = []\nlog = \"Info\"\nplat_dyn = \
                 true\n"
            ),
        )
        .unwrap();
        path
    }

    fn write_minimal_case(root: &Path, case_name: &str) {
        write_case_file(root, case_name, "init.sh", "echo hello\n");
        write_case_file(
            root,
            case_name,
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root,
            case_name,
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = true\n",
        );
    }

    #[test]
    fn resolves_board_case_from_apps_dir() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.name, "demo");
        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
        assert_eq!(case.init_cmd, "echo hello");
        assert!(
            case.board_config_path
                .ends_with("board-orangepi-5-plus.toml")
        );
        assert!(
            case.build_config_path
                .ends_with("build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn reports_missing_apps_dir() {
        let root = tempdir().unwrap();

        let err = resolve_board_case(root.path(), "demo", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("missing Starry apps directory"));
        assert!(err.contains("apps/starry"));
    }

    #[test]
    fn reports_unknown_case_with_available_cases() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");

        let err = resolve_board_case(root.path(), "missing", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown Starry app case `missing`"));
        assert!(err.contains("demo"));
    }

    #[test]
    fn reads_build_target_from_filename_when_toml_target_is_absent() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "env = {}\nfeatures = []\nlog = \"Info\"\nplat_dyn = true\n",
        );

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    }

    #[test]
    fn rejects_mismatched_build_target_filename() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"x86_64-unknown-none\"\nenv = {}\nfeatures = []\nlog = \"Info\"\nplat_dyn \
             = false\n",
        );

        let err = resolve_board_case(root.path(), "demo", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("does not match filename target"));
    }

    #[test]
    fn explicit_board_config_overrides_case_config() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");
        let explicit = root.path().join("custom-board.toml");
        fs::write(&explicit, "board_type = \"custom\"\n").unwrap();

        let case = resolve_board_case(root.path(), "demo", Some(explicit.as_path())).unwrap();

        assert_eq!(case.board_config_path, explicit);
    }

    #[test]
    fn explicit_relative_board_config_can_resolve_inside_case() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");
        let explicit = write_case_file(
            root.path(),
            "demo",
            "board-custom.toml",
            "board_type = \"Custom\"\nshell_prefix = \"root@starry:/root #\"\n",
        );

        let case =
            resolve_board_case(root.path(), "demo", Some(Path::new("board-custom.toml"))).unwrap();

        assert_eq!(case.board_config_path, explicit);
    }

    #[test]
    fn board_default_target_picks_matching_build_config() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = true\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-riscv64gc-unknown-none-elf.toml",
            "target = \"riscv64gc-unknown-none-elf\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = false\n",
        );
        write_board_default(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    }
    #[test]
    fn discovers_prebuild_apps_and_ignores_listed_names() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "codex-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash\n",
        );
        write_case_file(
            root.path(),
            "picoclaw-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash\n",
        );
        write_case_file(
            root.path(),
            "orangepi-5-plus-uvc",
            "prebuild.sh",
            "#!/usr/bin/env bash\n",
        );
        write_case_file(
            root.path(),
            "orangepi-5-plus-uvc-rknn",
            "prebuild.sh",
            "#!/usr/bin/env bash\n",
        );
        fs::write(
            root.path().join("apps/.ignore"),
            "apps/starry/orangepi-5-plus-uvc\napps/starry/orangepi-5-plus-uvc-rknn\n",
        )
        .unwrap();

        let apps = discover_apps(root.path()).unwrap();
        let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

        assert_eq!(names, vec!["codex-cli", "picoclaw-cli"]);
    }

    #[test]
    fn qemu_build_config_comes_from_app_dir() {
        let root = tempdir().unwrap();
        let build_config = write_case_file(
            root.path(),
            "codex-cli",
            "build-x86_64-unknown-none.toml",
            "target = \"x86_64-unknown-none\"
env = {}
features = []
log = \"Info\"
plat_dyn = false
",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash
",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "codex-cli")
            .unwrap();

        let selected = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
            .unwrap()
            .unwrap();

        assert_eq!(selected, build_config);
    }

    #[test]
    fn qemu_build_config_can_come_from_nearest_parent() {
        let root = tempdir().unwrap();
        let outer = write_case_file(
            root.path(),
            "qemu-smp1",
            "build-x86_64-unknown-none.toml",
            "target = \"x86_64-unknown-none\"
env = {}
features = []
log = \"Info\"
plat_dyn = false
",
        );
        let inner = write_case_file(
            root.path(),
            "qemu-smp1/nested",
            "build-x86_64-unknown-none.toml",
            "target = \"x86_64-unknown-none\"
env = {}
features = [\"nearest\"]
log = \"Info\"
plat_dyn = false
",
        );
        write_case_file(
            root.path(),
            "qemu-smp1/nested/codex-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash
",
        );
        write_case_file(
            root.path(),
            "qemu-smp1/nested/codex-cli",
            "qemu-x86_64.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "qemu-smp1/nested/codex-cli")
            .unwrap();

        let selected = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
            .unwrap()
            .unwrap();

        assert_eq!(selected, inner);
        assert_ne!(selected, outer);
    }

    #[test]
    fn qemu_build_config_rejects_legacy_arch_name() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "codex-cli",
            "build-x86_64.toml",
            "target = \"x86_64-unknown-none\"
env = {}
features = []
log = \"Info\"
plat_dyn = false
",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash
",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "codex-cli")
            .unwrap();

        let err = discover_optional_build_config(&app.case_dir, "x86_64-unknown-none")
            .unwrap_err()
            .to_string();

        assert!(err.contains("unsupported legacy build config name"));
        assert!(err.contains("build-x86_64.toml"));
    }

    #[test]
    fn qemu_config_selection_prefers_exact_arch_config() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64-codex-help.toml",
            "args = []
",
        );
        let exact = write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "codex-cli")
            .unwrap();

        let selected = resolve_qemu_config(&app, Some("x86_64"), None)
            .unwrap()
            .unwrap();

        assert_eq!(selected, exact);
    }

    #[test]
    fn qemu_config_selection_rejects_variant_only_default() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64-codex-help.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "codex-cli")
            .unwrap();

        let err = resolve_qemu_config(&app, Some("x86_64"), None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("qemu-x86_64.toml"));
    }

    #[test]
    fn qemu_config_selection_uses_explicit_variant_config() {
        let root = tempdir().unwrap();
        let explicit = write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64-codex-syscall-hunt.toml",
            "args = []
",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64-codex-help.toml",
            "args = []
",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "codex-cli")
            .unwrap();

        let selected = resolve_qemu_config(
            &app,
            Some("x86_64"),
            Some(Path::new("qemu-x86_64-codex-syscall-hunt.toml")),
        )
        .unwrap()
        .unwrap();

        assert_eq!(selected, explicit);
    }

    #[test]
    fn all_qemu_selection_skips_apps_without_matching_arch_config() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "qemu/apk-curl",
            "qemu-x86_64.toml",
            "args = []\n",
        );
        write_case_file(root.path(), "qemu/apt", "qemu-riscv64.toml", "args = []\n");
        let args = ArgsAppQemu {
            all: true,
            test_case: None,
            caps: Vec::new(),
            arch: Some("x86_64".to_string()),
            qemu_config: None,
            debug: false,
        };

        let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
        let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

        assert_eq!(names, vec!["qemu/apk-curl"]);
    }

    #[test]
    fn selected_qemu_case_allows_ignored_app_when_explicit() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "gdb-smoke", "qemu-riscv64.toml", "args = []\n");
        fs::write(root.path().join("apps/.ignore"), "apps/starry/gdb-smoke\n").unwrap();
        let args = ArgsAppQemu {
            all: false,
            test_case: Some("gdb-smoke".to_string()),
            caps: Vec::new(),
            arch: Some("riscv64".to_string()),
            qemu_config: None,
            debug: false,
        };

        let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
        let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

        assert_eq!(names, vec!["gdb-smoke"]);
    }

    #[test]
    fn selected_qemu_case_allows_ignored_nested_app_when_explicit() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "k230-qemu/qemu-k230/kpu-smoke",
            "qemu-riscv64.toml",
            "args = []\n",
        );
        fs::write(root.path().join("apps/.ignore"), "apps/starry/k230-qemu\n").unwrap();
        let args = ArgsAppQemu {
            all: false,
            test_case: Some("k230-qemu/qemu-k230/kpu-smoke".to_string()),
            caps: Vec::new(),
            arch: Some("riscv64".to_string()),
            qemu_config: None,
            debug: false,
        };

        let apps = selected_apps(root.path(), &args, StarryAppKind::Qemu).unwrap();
        let names = apps.into_iter().map(|app| app.name).collect::<Vec<_>>();

        assert_eq!(names, vec!["k230-qemu/qemu-k230/kpu-smoke"]);
    }

    #[test]
    fn qemu_case_fields_load_grouped_commands_and_subcases() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "qemu/sqlite",
            "qemu-x86_64.toml",
            "args = []\nuefi = false\nto_bin = true\nsuccess_regex = []\nfail_regex = \
             []\ntest_commands = [\"/usr/bin/app-sqlite\", \"/usr/bin/app-sqlite-deep\"]\n",
        );
        write_case_file(
            root.path(),
            "qemu/sqlite/app-sqlite/c",
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.20)\n",
        );
        write_case_file(
            root.path(),
            "qemu/sqlite/app-sqlite-deep/c",
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.20)\n",
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "qemu/sqlite")
            .unwrap();
        let qemu_config = resolve_qemu_config(&app, Some("x86_64"), None).unwrap();

        let fields =
            load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

        assert_eq!(
            fields.test_case.test_commands,
            vec!["/usr/bin/app-sqlite", "/usr/bin/app-sqlite-deep"]
        );
        assert_eq!(fields.test_case.subcases.len(), 2);
    }

    #[test]
    fn qemu_case_fields_load_configured_managed_rootfs() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs_path = root
            .path()
            .join(".tgos-images/rootfs-aarch64-debian.img/rootfs-aarch64-debian.img");
        write_case_file(
            root.path(),
            "qemu/apt",
            "qemu-aarch64.toml",
            r#"args = [
  "-drive",
  "id=disk0,if=none,format=raw,file=${workspace}/.tgos-images/rootfs-aarch64-debian.img/rootfs-aarch64-debian.img",
]
uefi = false
to_bin = true
success_regex = []
fail_regex = []
"#,
        );
        let app = discover_apps(root.path())
            .unwrap()
            .into_iter()
            .find(|app| app.name == "qemu/apt")
            .unwrap();
        let qemu_config = resolve_qemu_config(&app, Some("aarch64"), None).unwrap();

        let fields =
            load_qemu_app_case_fields(root.path(), &app, qemu_config.as_deref().unwrap()).unwrap();

        assert_eq!(fields.rootfs_path, Some(rootfs_path));
    }

    fn write_test_image_config(workspace_root: &Path) {
        let config = crate::image::config::ImageConfig {
            local_storage: workspace_root.join(".tgos-images"),
            registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        crate::image::config::ImageConfig::write_config(workspace_root, &config).unwrap();
    }

    #[test]
    fn app_qemu_test_case_preserves_host_symbolize_success_regex() {
        let case_dir = PathBuf::from("/tmp/apps/starry/memtrack-backtrace");
        let qemu_config_path = case_dir.join("qemu-x86_64.toml");
        let case = StarryAppQemuCase {
            name: "memtrack-backtrace".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            build_config_path: None,
            qemu_config_path: Some(qemu_config_path.clone()),
            rootfs_path: PathBuf::from("/tmp/rootfs.img"),
            test_commands: Vec::new(),
            host_symbolize_success_regex: vec!["symbolized".to_string()],
            subcases: Vec::new(),
        };

        let test_case = app_qemu_test_case(&case, case_dir.clone()).unwrap();

        assert_eq!(test_case.case_dir, case_dir);
        assert_eq!(test_case.qemu_config_path, qemu_config_path);
        assert_eq!(test_case.host_symbolize_success_regex, vec!["symbolized"]);
    }

    #[test]
    fn infers_qemu_and_board_app_kinds() {
        let root = tempdir().unwrap();
        write_case_file(
            root.path(),
            "codex-cli",
            "prebuild.sh",
            "#!/usr/bin/env bash\n",
        );
        write_case_file(
            root.path(),
            "codex-cli",
            "qemu-x86_64-codex-help.toml",
            "args = []\n",
        );
        write_minimal_case(root.path(), "board-demo");

        let apps = discover_apps(root.path()).unwrap();

        assert_eq!(apps[0].name, "board-demo");
        assert_eq!(apps[0].kind, StarryAppKind::Board);
        assert_eq!(apps[1].name, "codex-cli");
        assert_eq!(apps[1].kind, StarryAppKind::Qemu);
    }
}
