use super::*;

pub(crate) fn cross_compile_spec(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    crate::context::cross_compile_spec_for_arch_checked(arch)
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

    let mut content = include_str!("../cmake-toolchain.cmake.in").to_string();
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

pub(super) fn cmake_value(value: impl AsRef<std::ffi::OsStr>) -> String {
    value.as_ref().to_string_lossy().replace('\\', "/")
}

pub(super) fn detect_gcc_runtime_dir(sysroot: &Path, guest_tool_dir: &str) -> Option<PathBuf> {
    let triplet = Path::new(guest_tool_dir).parent()?.file_name()?;
    let gcc_root = sysroot.join("usr/lib/gcc").join(triplet);
    let entries = fs::read_dir(&gcc_root).ok()?;
    let runtime_dirs = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();

    runtime_dirs
        .iter()
        .filter_map(|path| {
            let dir_name = path.file_name()?.to_str()?;
            let version = parse_gcc_runtime_version(dir_name)?;
            Some((version, path))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)))
        .map(|(_, path)| path.clone())
        .or_else(|| runtime_dirs.into_iter().max())
}

pub(super) fn parse_gcc_runtime_version(dir_name: &str) -> Option<Vec<u64>> {
    let mut version = Vec::new();
    for segment in dir_name.split('.') {
        if segment.is_empty() {
            return None;
        }
        let digits = segment
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if digits.is_empty() {
            return None;
        }
        version.push(digits.parse().ok()?);
    }
    Some(version)
}
