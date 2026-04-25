//! StarryOS C test case build orchestration.
//!
//! Main responsibilities:
//! - Prepare guest prebuild and host cross-build environments for C cases
//! - Generate toolchain and wrapper scripts used during case builds
//! - Run prebuild scripts and CMake configure/build/install steps
//! - Populate case overlays that will later be injected into the rootfs image

use std::{
    collections::BTreeMap,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail, ensure};

use super::{case_assets, test_suit::StarryQemuCase};
use crate::process::ProcessExt;

const CASE_C_DIR_NAME: &str = "c";
const CASE_PREBUILD_SCRIPT_NAME: &str = "prebuild.sh";
const CASE_CMAKE_FILE_NAME: &str = "CMakeLists.txt";
pub(crate) const STARRY_APK_REGION_VAR: &str = "STARRY_APK_REGION";
pub(crate) const STARRY_STAGING_ROOT_VAR: &str = "STARRY_STAGING_ROOT";
pub(crate) const STARRY_CASE_DIR_VAR: &str = "STARRY_CASE_DIR";
pub(crate) const STARRY_CASE_C_DIR_VAR: &str = "STARRY_CASE_C_DIR";
pub(crate) const STARRY_CASE_WORK_DIR_VAR: &str = "STARRY_CASE_WORK_DIR";
pub(crate) const STARRY_CASE_BUILD_DIR_VAR: &str = "STARRY_CASE_BUILD_DIR";
pub(crate) const STARRY_CASE_OVERLAY_DIR_VAR: &str = "STARRY_CASE_OVERLAY_DIR";
const HOST_RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const HOST_RESOLVED_CONF_PATH: &str = "/run/systemd/resolve/resolv.conf";
const DEFAULT_DNS_SERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];
const CROSS_BINUTILS: &[&str] = &[
    "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
];
const CHINA_ALPINE_MIRROR: &str = "https://mirrors.cernet.edu.cn/alpine";
const US_ALPINE_MIRROR: &str = "https://dl-cdn.alpinelinux.org/alpine";

#[derive(Debug, Clone)]
pub(crate) struct HostCrossBuildEnv {
    cmake: PathBuf,
    pkg_config: PathBuf,
    make_program: PathBuf,
    cmake_toolchain_file: PathBuf,
    command_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossCompileSpec {
    llvm_target: &'static str,
    cmake_system_processor: &'static str,
    guest_tool_dir: &'static str,
    gnu_tool_prefix: &'static str,
}

#[derive(Debug, Clone)]
pub(crate) struct GuestPrebuildEnv {
    qemu_runner: PathBuf,
    script_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApkRegion {
    China,
    Us,
}

impl ApkRegion {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::China => "china",
            Self::Us => "us",
        }
    }

    fn mirror_base(self) -> &'static str {
        match self {
            Self::China => CHINA_ALPINE_MIRROR,
            Self::Us => US_ALPINE_MIRROR,
        }
    }
}

/// Returns the C source directory for a Starry test case.
pub(crate) fn case_c_source_dir(case: &StarryQemuCase) -> PathBuf {
    case.case_dir.join(CASE_C_DIR_NAME)
}

/// Returns the optional prebuild script path for a Starry C case.
pub(crate) fn case_prebuild_script_path(case: &StarryQemuCase) -> PathBuf {
    case_c_source_dir(case).join(CASE_PREBUILD_SCRIPT_NAME)
}

/// Prepares rootfs-backed assets for a Starry C-based test case.
pub(crate) fn prepare_c_case_assets_sync(
    arch: &str,
    case: &StarryQemuCase,
    case_rootfs: &Path,
    layout: &case_assets::CaseAssetLayout,
) -> anyhow::Result<()> {
    let source_dir = case_c_source_dir(case);
    let cmake_lists = source_dir.join(CASE_CMAKE_FILE_NAME);
    ensure!(
        cmake_lists.is_file(),
        "missing case CMake project entry `{}`",
        cmake_lists.display()
    );

    case_assets::reset_dir(&layout.staging_root)?;
    case_assets::reset_dir(&layout.build_dir)?;
    case_assets::reset_dir(&layout.overlay_dir)?;
    case_assets::reset_dir(&layout.command_wrapper_dir)?;
    case_assets::reset_dir(&layout.cross_bin_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    crate::rootfs::inject::extract_rootfs(case_rootfs, &layout.staging_root)?;
    write_host_resolver_config(&layout.staging_root)?;
    let prebuild_script = case_prebuild_script_path(case);
    if prebuild_script.is_file() {
        let apk_region = apk_region_from_env()?;
        rewrite_apk_repositories_for_region(&layout.staging_root, apk_region)?;
        log_apk_prebuild_context(&layout.staging_root, apk_region)?;
        let prebuild_env = prepare_guest_prebuild_env(arch, case, layout, apk_region)?;
        let mut command = build_prebuild_command(case, &prebuild_script, layout, &prebuild_env)?;
        command.exec().context("failed to run case prebuild.sh")?;
    }
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    let build_env = prepare_host_cross_build_env(arch, layout, &qemu_runner)?;

    let mut configure = build_cmake_configure_command(case, layout, &build_env);
    configure
        .exec()
        .context("failed to configure case C project")?;

    let mut build = build_cmake_build_command(layout, &build_env);
    build.exec().context("failed to build case C project")?;

    let mut install = build_cmake_install_command(layout, &build_env);
    install.exec().context("failed to install case C project")?;

    crate::rootfs::runtime::sync_runtime_dependencies(&layout.staging_root, &layout.overlay_dir)?;
    crate::rootfs::inject::inject_overlay(case_rootfs, &layout.overlay_dir)
}

fn write_host_resolver_config(staging_root: &Path) -> anyhow::Result<()> {
    let resolv_conf = preferred_host_resolver_config()?;
    let output_path = staging_root.join("etc/resolv.conf");
    fs::write(&output_path, resolv_conf)
        .with_context(|| format!("failed to write {}", output_path.display()))
}

fn preferred_host_resolver_config() -> anyhow::Result<String> {
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLVED_CONF_PATH))? {
        return Ok(content);
    }
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLV_CONF_PATH))? {
        return Ok(content);
    }

    Ok(DEFAULT_DNS_SERVERS
        .iter()
        .map(|server| format!("nameserver {server}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn read_usable_resolver_file(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let usable = content
        .lines()
        .filter_map(parse_nameserver_line)
        .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
        .map(|addr| format!("nameserver {addr}"))
        .collect::<Vec<_>>();

    if usable.is_empty() {
        Ok(None)
    } else {
        Ok(Some(usable.join("\n") + "\n"))
    }
}

pub(crate) fn parse_nameserver_line(line: &str) -> Option<IpAddr> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("nameserver"), Some(value), None) => value.parse().ok(),
        _ => None,
    }
}

fn apk_region_from_env() -> anyhow::Result<ApkRegion> {
    let value = std::env::var(STARRY_APK_REGION_VAR).ok();
    parse_apk_region(value.as_deref())
}

pub(crate) fn parse_apk_region(value: Option<&str>) -> anyhow::Result<ApkRegion> {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "china" | "cn") => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "us" | "usa") => Ok(ApkRegion::Us),
        Some(value) => bail!(
            "unsupported {STARRY_APK_REGION_VAR} `{value}`; supported values are: china, cn, us, \
             usa"
        ),
    }
}

fn rewrite_apk_repositories_for_region(
    staging_root: &Path,
    region: ApkRegion,
) -> anyhow::Result<()> {
    let path = staging_root.join("etc/apk/repositories");
    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let ends_with_newline = original.ends_with('\n');
    let rewritten = original
        .lines()
        .map(|line| rewrite_apk_repository_line(line, region))
        .collect::<Vec<_>>()
        .join("\n");

    let mut output = rewritten;
    if ends_with_newline {
        output.push('\n');
    }

    fs::write(&path, output).with_context(|| format!("failed to write {}", path.display()))
}

fn rewrite_apk_repository_line(line: &str, region: ApkRegion) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return line.to_string();
    }

    let Some((_, suffix)) = trimmed.split_once("/alpine/") else {
        return line.to_string();
    };

    let leading_len = line.len() - line.trim_start().len();
    let trailing_len = line.len() - line.trim_end().len();
    let trailing = if trailing_len == 0 {
        ""
    } else {
        &line[line.len() - trailing_len..]
    };

    format!(
        "{}{}/{}{}",
        &line[..leading_len],
        region.mirror_base(),
        suffix,
        trailing
    )
}

fn log_apk_prebuild_context(staging_root: &Path, region: ApkRegion) -> anyhow::Result<()> {
    let repositories_path = staging_root.join("etc/apk/repositories");
    let repositories = fs::read_to_string(&repositories_path)
        .with_context(|| format!("failed to read {}", repositories_path.display()))?;

    println!("STARRY_APK_REGION={}", region.canonical_name());
    println!("apk repositories:");
    print!("{repositories}");
    if !repositories.ends_with('\n') {
        println!();
    }

    Ok(())
}

fn prepare_guest_prebuild_env(
    arch: &str,
    case: &StarryQemuCase,
    layout: &case_assets::CaseAssetLayout,
    apk_region: ApkRegion,
) -> anyhow::Result<GuestPrebuildEnv> {
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_guest_command_wrappers(layout, &qemu_runner)?;

    let mut script_envs = case_script_envs(case, layout);
    script_envs.push((
        STARRY_APK_REGION_VAR.to_string(),
        apk_region.canonical_name().to_string(),
    ));

    Ok(GuestPrebuildEnv {
        qemu_runner,
        script_envs,
    })
}

fn prepare_host_cross_build_env(
    arch: &str,
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<HostCrossBuildEnv> {
    let spec = cross_compile_spec(arch)?;
    let cmake = find_host_binary_candidates(&["cmake"])?;
    let clang = find_host_binary_candidates(&["clang"])?;
    let pkg_config = find_host_binary_candidates(&["pkg-config"])?;
    let make_program = find_host_binary_candidates(&["make", "gmake"])?;

    write_cross_bin_wrappers(layout, spec, qemu_runner)?;
    write_cmake_toolchain_file(layout, spec, &clang)?;

    let pkgconfig_libdir = format!(
        "{}:{}",
        layout.staging_root.join("usr/lib/pkgconfig").display(),
        layout.staging_root.join("usr/share/pkgconfig").display()
    );
    let command_envs = vec![
        ("PKG_CONFIG_LIBDIR".to_string(), pkgconfig_libdir),
        (
            "PKG_CONFIG_SYSROOT_DIR".to_string(),
            layout.staging_root.display().to_string(),
        ),
        ("PKG_CONFIG_PATH".to_string(), String::new()),
    ];

    Ok(HostCrossBuildEnv {
        cmake,
        pkg_config,
        make_program,
        cmake_toolchain_file: layout.cmake_toolchain_file.clone(),
        command_envs,
    })
}

pub(crate) fn cross_compile_spec(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    match arch {
        "aarch64" => Ok(CrossCompileSpec {
            llvm_target: "aarch64-linux-musl",
            cmake_system_processor: "aarch64",
            guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "aarch64-linux-musl",
        }),
        "riscv64" => Ok(CrossCompileSpec {
            llvm_target: "riscv64-linux-musl",
            cmake_system_processor: "riscv64",
            guest_tool_dir: "usr/riscv64-alpine-linux-musl/bin",
            gnu_tool_prefix: "riscv64-linux-musl",
        }),
        "x86_64" => Ok(CrossCompileSpec {
            llvm_target: "x86_64-linux-musl",
            cmake_system_processor: "x86_64",
            guest_tool_dir: "usr/x86_64-alpine-linux-musl/bin",
            gnu_tool_prefix: "x86_64-linux-musl",
        }),
        "loongarch64" => Ok(CrossCompileSpec {
            llvm_target: "loongarch64-linux-musl",
            cmake_system_processor: "loongarch64",
            guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "loongarch64-linux-musl",
        }),
        _ => bail!(
            "Starry C test cases are only supported on aarch64, riscv64, x86_64, and loongarch64, \
             but got `{arch}`"
        ),
    }
}

pub(crate) fn write_cross_bin_wrappers(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(&layout.cross_bin_dir)
        .with_context(|| format!("failed to create {}", layout.cross_bin_dir.display()))?;
    for tool in CROSS_BINUTILS {
        let guest_relative_path = format!("{}/{tool}", spec.guest_tool_dir);
        ensure_guest_tool_exists(&layout.staging_root, &guest_relative_path)?;
        write_guest_exec_wrapper(
            &layout.cross_bin_dir.join(tool),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
        write_guest_exec_wrapper(
            &layout
                .cross_bin_dir
                .join(format!("{}-{tool}", spec.gnu_tool_prefix)),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
    }

    Ok(())
}

pub(crate) fn write_cmake_toolchain_file(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
    clang: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = layout.cmake_toolchain_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let sysroot = &layout.staging_root;
    let gcc_toolchain_root = sysroot.join("usr");
    let mut compile_flags = vec![
        format!("--sysroot={}", sysroot.display()),
        format!("--gcc-toolchain={}", gcc_toolchain_root.display()),
        format!("-B{}", layout.cross_bin_dir.display()),
    ];
    let mut linker_flags = compile_flags.clone();
    if let Some(gcc_runtime_dir) = detect_gcc_runtime_dir(sysroot, spec.guest_tool_dir) {
        // Older host clang may miss Alpine GCC runtime dirs unless explicitly provided.
        compile_flags.push(format!("-B{}", gcc_runtime_dir.display()));
        linker_flags = compile_flags.clone();
        linker_flags.push(format!("-L{}", gcc_runtime_dir.display()));
    }
    let compile_flags = compile_flags.join(" ");
    let linker_flags = linker_flags.join(" ");

    let mut content = include_str!("cmake-toolchain.cmake.in").to_string();
    for (needle, value) in [
        (
            "@CMAKE_SYSTEM_PROCESSOR@",
            spec.cmake_system_processor.to_string(),
        ),
        ("@CMAKE_SYSROOT@", cmake_value(sysroot)),
        ("@CMAKE_FIND_ROOT_PATH@", cmake_value(sysroot)),
        ("@CMAKE_C_COMPILER@", cmake_value(clang)),
        ("@CMAKE_C_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_ASM_COMPILER@", cmake_value(clang)),
        ("@CMAKE_ASM_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_AR@", cmake_value(layout.cross_bin_dir.join("ar"))),
        (
            "@CMAKE_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        (
            "@CMAKE_STRIP@",
            cmake_value(layout.cross_bin_dir.join("strip")),
        ),
        (
            "@CMAKE_LINKER@",
            cmake_value(layout.cross_bin_dir.join("ld")),
        ),
        ("@CMAKE_NM@", cmake_value(layout.cross_bin_dir.join("nm"))),
        (
            "@CMAKE_OBJCOPY@",
            cmake_value(layout.cross_bin_dir.join("objcopy")),
        ),
        (
            "@CMAKE_OBJDUMP@",
            cmake_value(layout.cross_bin_dir.join("objdump")),
        ),
        (
            "@CMAKE_READELF@",
            cmake_value(layout.cross_bin_dir.join("readelf")),
        ),
        (
            "@CMAKE_C_COMPILER_AR@",
            cmake_value(layout.cross_bin_dir.join("ar")),
        ),
        (
            "@CMAKE_C_COMPILER_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        ("@CMAKE_C_FLAGS_INIT@", cmake_value(&compile_flags)),
        ("@CMAKE_ASM_FLAGS_INIT@", cmake_value(&compile_flags)),
        ("@CMAKE_LINKER_FLAGS_INIT@", cmake_value(&linker_flags)),
    ] {
        content = content.replace(needle, &value);
    }

    fs::write(&layout.cmake_toolchain_file, content)
        .with_context(|| format!("failed to write {}", layout.cmake_toolchain_file.display()))
}

fn cmake_value(value: impl AsRef<std::ffi::OsStr>) -> String {
    value.as_ref().to_string_lossy().replace('\\', "/")
}

fn detect_gcc_runtime_dir(sysroot: &Path, guest_tool_dir: &str) -> Option<PathBuf> {
    let triplet = Path::new(guest_tool_dir).parent()?.file_name()?;
    let gcc_root = sysroot.join("usr/lib/gcc").join(triplet);
    let entries = fs::read_dir(&gcc_root).ok()?;
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .max()
}

pub(crate) fn build_prebuild_command(
    case: &StarryQemuCase,
    prebuild_script: &Path,
    layout: &case_assets::CaseAssetLayout,
    prebuild_env: &GuestPrebuildEnv,
) -> anyhow::Result<Command> {
    let guest_busybox = layout.staging_root.join("bin/busybox");
    let guest_shell = layout.staging_root.join("bin/sh");
    let mut command = Command::new(&prebuild_env.qemu_runner);
    command.arg("-L").arg(&layout.staging_root);
    if guest_busybox.is_file() {
        command.arg(&guest_busybox).arg("sh");
    } else {
        ensure!(
            guest_shell.is_file(),
            "staging root is missing guest shell `{}`",
            guest_shell.display()
        );
        command.arg(&guest_shell);
    }
    command
        .arg("-eu")
        .arg(prebuild_script)
        .current_dir(case_c_source_dir(case));
    apply_case_script_envs(&mut command, layout, &prebuild_env.script_envs);
    Ok(command)
}

pub(crate) fn build_cmake_configure_command(
    case: &StarryQemuCase,
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("-S")
        .arg(case_c_source_dir(case))
        .arg("-B")
        .arg(&layout.build_dir)
        .arg("-G")
        .arg("Unix Makefiles")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_INSTALL_PREFIX=/")
        .arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY")
        .arg(format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        ))
        .arg(format!(
            "-DCMAKE_MAKE_PROGRAM={}",
            build_env.make_program.display()
        ))
        .arg(format!(
            "-DPKG_CONFIG_EXECUTABLE={}",
            build_env.pkg_config.display()
        ))
        .arg(format!(
            "-D{STARRY_STAGING_ROOT_VAR}={}",
            layout.staging_root.display()
        ));

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_cmake_build_command(
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("--build")
        .arg(&layout.build_dir)
        .arg("--parallel");

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_cmake_install_command(
    layout: &case_assets::CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command.arg("--install").arg(&layout.build_dir);
    command.env("DESTDIR", &layout.overlay_dir);

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn apply_case_script_envs(
    command: &mut Command,
    layout: &case_assets::CaseAssetLayout,
    script_envs: &[(String, String)],
) {
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));

    command.env("PATH", std::env::join_paths(path_entries).unwrap());
    command.env("QEMU_LD_PREFIX", &layout.staging_root);

    for (key, value) in script_envs {
        command.env(key, value);
    }
}

pub(crate) fn case_script_envs(
    case: &StarryQemuCase,
    layout: &case_assets::CaseAssetLayout,
) -> Vec<(String, String)> {
    vec![
        (
            STARRY_STAGING_ROOT_VAR.to_string(),
            layout.staging_root.display().to_string(),
        ),
        (
            STARRY_CASE_DIR_VAR.to_string(),
            case.case_dir.display().to_string(),
        ),
        (
            STARRY_CASE_C_DIR_VAR.to_string(),
            case_c_source_dir(case).display().to_string(),
        ),
        (
            STARRY_CASE_WORK_DIR_VAR.to_string(),
            layout.work_dir.display().to_string(),
        ),
        (
            STARRY_CASE_BUILD_DIR_VAR.to_string(),
            layout.build_dir.display().to_string(),
        ),
        (
            STARRY_CASE_OVERLAY_DIR_VAR.to_string(),
            layout.overlay_dir.display().to_string(),
        ),
    ]
}

fn write_guest_command_wrappers(
    layout: &case_assets::CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    let mut guest_commands = BTreeMap::new();
    for relative_dir in ["bin", "sbin", "usr/bin", "usr/sbin"] {
        let dir_path = layout.staging_root.join(relative_dir);
        if !dir_path.is_dir() {
            continue;
        }

        let mut entries = fs::read_dir(&dir_path)
            .with_context(|| format!("failed to read {}", dir_path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read {}", dir_path.display()))?;
        entries.sort_by_key(|left| left.file_name());

        for entry in entries {
            let name = entry.file_name();
            if guest_commands.contains_key(name.as_os_str()) {
                continue;
            }

            let file_type = entry.file_type().with_context(|| {
                format!("failed to inspect guest command {}", entry.path().display())
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            guest_commands.insert(
                name,
                format!("{relative_dir}/{}", entry.file_name().to_string_lossy()),
            );
        }
    }

    for (name, relative_guest_path) in guest_commands {
        let wrapper_path = layout.command_wrapper_dir.join(&name);
        if relative_guest_path == "sbin/apk" {
            write_apk_wrapper_script(&wrapper_path, qemu_runner, &layout.staging_root, layout)?;
        } else {
            write_guest_exec_wrapper(
                &wrapper_path,
                qemu_runner,
                &layout.staging_root,
                &relative_guest_path,
                None,
            )?;
        }
    }

    Ok(())
}

fn ensure_guest_tool_exists(staging_root: &Path, relative_path: &str) -> anyhow::Result<()> {
    let path = staging_root.join(relative_path);
    ensure!(
        path.is_file(),
        "staging root is missing required guest tool `{}`; install it in prebuild.sh",
        path.display()
    );
    Ok(())
}

pub(crate) fn qemu_user_binary_names(arch: &str) -> anyhow::Result<&'static [&'static str]> {
    match arch {
        "aarch64" => Ok(&["qemu-aarch64-static", "qemu-aarch64"]),
        "riscv64" => Ok(&["qemu-riscv64-static", "qemu-riscv64"]),
        "x86_64" => Ok(&["qemu-x86_64-static", "qemu-x86_64"]),
        "loongarch64" => Ok(&["qemu-loongarch64-static", "qemu-loongarch64"]),
        _ => bail!(
            "Starry C test cases are only supported on aarch64, riscv64, x86_64, and loongarch64, \
             but got `{arch}`"
        ),
    }
}

fn write_guest_exec_wrapper(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    guest_relative_path: &str,
    extra_args: Option<String>,
) -> anyhow::Result<()> {
    let guest_path = staging_root.join(guest_relative_path);
    let mut body = format!(
        "export QEMU_LD_PREFIX={root}\nexec {qemu} -0 {guest} -L {root} {guest}",
        root = shell_single_quote(staging_root),
        qemu = shell_single_quote(qemu_runner),
        guest = shell_single_quote(&guest_path),
    );
    if let Some(extra_args) = extra_args {
        body.push(' ');
        body.push_str(&extra_args);
    }
    body.push_str(" \"$@\"\n");

    write_wrapper_script(path, &body)
}

fn write_apk_wrapper_script(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    layout: &case_assets::CaseAssetLayout,
) -> anyhow::Result<()> {
    let body = format!(
        "export QEMU_LD_PREFIX={root}\nexec {qemu} -L {root} {apk} --root {root} \
         --repositories-file {repositories} --keys-dir {keys} --cache-dir {cache} --update-cache \
         --timeout 60 --no-interactive --force-no-chroot --scripts=no \"$@\"\n",
        root = shell_single_quote(staging_root),
        qemu = shell_single_quote(qemu_runner),
        apk = shell_single_quote(staging_root.join("sbin/apk")),
        repositories = shell_single_quote(staging_root.join("etc/apk/repositories")),
        keys = shell_single_quote(staging_root.join("etc/apk/keys")),
        cache = shell_single_quote(&layout.apk_cache_dir),
    );
    write_wrapper_script(path, &body)
}

fn write_wrapper_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .with_context(|| format!("failed to stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod {}", path.display()))?;
    }
    Ok(())
}

fn find_host_binary_candidates(candidates: &[&str]) -> anyhow::Result<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_optional_host_binary(candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "required host binary was not found in PATH; tried: {}",
                candidates.join(", ")
            )
        })
}

fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn shell_single_quote(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().display().to_string().replace('\'', "'\\''");
    format!("'{value}'")
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::PathBuf, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn fake_case(root: &Path, name: &str) -> StarryQemuCase {
        let case_dir = root.join("test-suit/starryos/normal").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        StarryQemuCase {
            name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
            build_config_path: None,
        }
    }

    fn command_env(command: &Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            (name == OsStr::new(key))
                .then(|| value.map(|value| value.to_string_lossy().into_owned()))
                .flatten()
        })
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_prebuild_command_uses_guest_shell_and_case_envs() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(layout.staging_root.join("bin")).unwrap();
        fs::write(layout.staging_root.join("bin/sh"), b"").unwrap();
        fs::write(layout.staging_root.join("bin/busybox"), b"").unwrap();
        let prebuild_env = GuestPrebuildEnv {
            qemu_runner: PathBuf::from("/usr/bin/qemu-aarch64-static"),
            script_envs: {
                let mut envs = case_script_envs(&case, &layout);
                envs.push((STARRY_APK_REGION_VAR.to_string(), "us".to_string()));
                envs
            },
        };
        let prebuild_script = case_c_source_dir(&case).join("prebuild.sh");

        let command =
            build_prebuild_command(&case, &prebuild_script, &layout, &prebuild_env).unwrap();

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/qemu-aarch64-static")
        );
        assert_eq!(
            command_args(&command),
            vec![
                "-L".to_string(),
                layout.staging_root.display().to_string(),
                layout
                    .staging_root
                    .join("bin/busybox")
                    .display()
                    .to_string(),
                "sh".to_string(),
                "-eu".to_string(),
                prebuild_script.display().to_string(),
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(case_c_source_dir(&case).as_path())
        );
        assert_eq!(
            command_env(&command, STARRY_CASE_OVERLAY_DIR_VAR),
            Some(layout.overlay_dir.display().to_string())
        );
        assert_eq!(
            command_env(&command, STARRY_APK_REGION_VAR),
            Some("us".to_string())
        );
    }

    #[test]
    fn cmake_configure_command_passes_staging_root_define() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        let build_env = HostCrossBuildEnv {
            cmake: PathBuf::from("/usr/bin/cmake"),
            pkg_config: PathBuf::from("/usr/bin/pkg-config"),
            make_program: PathBuf::from("/usr/bin/make"),
            cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
            command_envs: vec![("PKG_CONFIG_LIBDIR".to_string(), "/sysroot".to_string())],
        };

        let command = build_cmake_configure_command(&case, &layout, &build_env);
        let args = command_args(&command);

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/cmake")
        );
        assert!(args.contains(&format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        )));
        assert!(args.contains(&format!(
            "-D{STARRY_STAGING_ROOT_VAR}={}",
            layout.staging_root.display()
        )));
        assert_eq!(
            command_env(&command, "PKG_CONFIG_LIBDIR"),
            Some("/sysroot".to_string())
        );
    }

    #[test]
    fn cross_compile_spec_maps_supported_arches() {
        assert_eq!(
            cross_compile_spec("aarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "aarch64-linux-musl",
                cmake_system_processor: "aarch64",
                guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "aarch64-linux-musl",
            }
        );
        assert_eq!(
            cross_compile_spec("loongarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "loongarch64-linux-musl",
                cmake_system_processor: "loongarch64",
                guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "loongarch64-linux-musl",
            }
        );
    }

    #[test]
    fn write_cross_bin_wrappers_generates_prefixed_and_plain_tools() {
        let root = tempdir().unwrap();
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(
            layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin"),
        )
        .unwrap();
        for tool in [
            "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
        ] {
            let path = layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin")
                .join(tool);
            fs::write(path, b"").unwrap();
        }

        write_cross_bin_wrappers(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/qemu-aarch64-static"),
        )
        .unwrap();

        let plain = fs::read_to_string(layout.cross_bin_dir.join("ld")).unwrap();
        let prefixed =
            fs::read_to_string(layout.cross_bin_dir.join("aarch64-linux-musl-ld")).unwrap();
        assert!(plain.contains("qemu-aarch64-static"));
        assert!(plain.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("-0"));
    }

    #[test]
    fn write_cmake_toolchain_file_contains_clang_cross_settings() {
        let root = tempdir().unwrap();
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();
        fs::create_dir_all(&layout.cross_bin_dir).unwrap();
        fs::create_dir_all(
            layout
                .staging_root
                .join("usr/lib/gcc/aarch64-alpine-linux-musl/15.2.0"),
        )
        .unwrap();

        write_cmake_toolchain_file(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/clang"),
        )
        .unwrap();

        let content = fs::read_to_string(&layout.cmake_toolchain_file).unwrap();
        assert!(content.contains("set(CMAKE_SYSTEM_NAME Linux)"));
        assert!(content.contains("set(CMAKE_C_COMPILER \"/usr/bin/clang\")"));
        assert!(content.contains("set(CMAKE_C_COMPILER_TARGET \"aarch64-linux-musl\")"));
        assert!(content.contains("--gcc-toolchain="));
        assert!(content.contains("-B"));
        assert!(content.contains("-L"));
        assert!(content.contains("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER"));
    }

    #[test]
    fn qemu_user_binary_names_cover_supported_arches() {
        assert_eq!(
            qemu_user_binary_names("aarch64").unwrap(),
            &["qemu-aarch64-static", "qemu-aarch64"]
        );
        assert_eq!(
            qemu_user_binary_names("riscv64").unwrap(),
            &["qemu-riscv64-static", "qemu-riscv64"]
        );
        assert_eq!(
            qemu_user_binary_names("x86_64").unwrap(),
            &["qemu-x86_64-static", "qemu-x86_64"]
        );
        assert_eq!(
            qemu_user_binary_names("loongarch64").unwrap(),
            &["qemu-loongarch64-static", "qemu-loongarch64"]
        );
    }

    #[test]
    fn preferred_resolver_filters_loopback_and_slirp_addresses() {
        let content = "nameserver 127.0.0.53\nnameserver 10.0.2.3\nnameserver 8.8.8.8\n";
        let usable = content
            .lines()
            .filter_map(parse_nameserver_line)
            .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
            .map(|addr| format!("nameserver {addr}"))
            .collect::<Vec<_>>();
        assert_eq!(usable, vec!["nameserver 8.8.8.8".to_string()]);
    }

    #[test]
    fn case_script_envs_include_expected_paths() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_assets::case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb")
                .unwrap();

        let envs = case_script_envs(&case, &layout);

        assert!(envs.contains(&(
            STARRY_CASE_DIR_VAR.to_string(),
            case.case_dir.display().to_string()
        )));
        assert!(envs.contains(&(
            STARRY_CASE_BUILD_DIR_VAR.to_string(),
            layout.build_dir.display().to_string()
        )));
    }

    #[test]
    fn parse_apk_region_defaults_to_china() {
        assert_eq!(parse_apk_region(None).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("")).unwrap(), ApkRegion::China);
    }

    #[test]
    fn parse_apk_region_accepts_supported_aliases() {
        assert_eq!(parse_apk_region(Some("china")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("cn")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("us")).unwrap(), ApkRegion::Us);
        assert_eq!(parse_apk_region(Some("usa")).unwrap(), ApkRegion::Us);
    }

    #[test]
    fn parse_apk_region_rejects_unknown_value() {
        let err = parse_apk_region(Some("europe")).unwrap_err().to_string();
        assert!(err.contains(STARRY_APK_REGION_VAR));
        assert!(err.contains("china, cn, us, usa"));
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_us_mirror() {
        let input = "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n";
        let output = input
            .lines()
            .map(|line| rewrite_apk_repository_line(line, ApkRegion::Us))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output,
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community"
        );
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_china_mirror() {
        let input = "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n";
        let output = input
            .lines()
            .map(|line| rewrite_apk_repository_line(line, ApkRegion::China))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output,
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community"
        );
    }

    #[test]
    fn rewrite_apk_repositories_for_region_updates_file() {
        let root = tempdir().unwrap();
        let repositories = root.path().join("etc/apk/repositories");
        fs::create_dir_all(repositories.parent().unwrap()).unwrap();
        fs::write(
            &repositories,
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n",
        )
        .unwrap();

        rewrite_apk_repositories_for_region(root.path(), ApkRegion::Us).unwrap();

        assert_eq!(
            fs::read_to_string(&repositories).unwrap(),
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n"
        );
    }

    #[test]
    fn format_duration_like_summary_helpers_are_precise_enough() {
        assert_eq!(
            format!("{:.2}", Duration::from_millis(1250).as_secs_f64()),
            "1.25"
        );
    }
}
