use super::*;

pub(crate) fn cross_compile_spec(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    crate::context::cross_compile_spec_for_arch_checked(arch)
}

/// Shares provider selection between wrapper generation and asset caching.
pub(crate) fn find_cross_tool_qemu(
    spec: CrossCompileSpec,
    mut find: impl FnMut(&str) -> Option<PathBuf>,
) -> Option<PathBuf> {
    spec.qemu_user_binaries.iter().find_map(|name| find(name))
}

/// Generates the same cross-tool names for emulated and host-native binutils.
pub(crate) fn write_cross_bin_wrappers(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
) -> anyhow::Result<()> {
    write_cross_bin_wrappers_with_lookup(
        layout,
        spec,
        crate::support::process::find_optional_host_binary,
    )
}

fn write_cross_bin_wrappers_with_lookup(
    layout: &case_assets::CaseAssetLayout,
    spec: CrossCompileSpec,
    mut find: impl FnMut(&str) -> Option<PathBuf>,
) -> anyhow::Result<()> {
    let qemu_runner = find_cross_tool_qemu(spec, &mut find);
    fs::create_dir_all(&layout.cross_bin_dir)
        .with_context(|| format!("failed to create {}", layout.cross_bin_dir.display()))?;
    for tool in CROSS_BINUTILS {
        let prefixed = format!("{}-{tool}", spec.gnu_tool_prefix);
        let guest_relative_path = format!("{}/{tool}", spec.guest_tool_dir);
        let tool_path = if qemu_runner.is_some() {
            ensure_guest_tool_exists(&layout.staging_root, &guest_relative_path)?;
            layout.staging_root.join(&guest_relative_path)
        } else {
            find(&prefixed).with_context(|| {
                format!(
                    "no qemu-user ({}); required native cross tool `{prefixed}` was not found; \
                     install qemu-user or the {} cross binutils",
                    spec.qemu_user_binaries.join(", "),
                    spec.gnu_tool_prefix,
                )
            })?
        };
        for name in [*tool, prefixed.as_str()] {
            let path = layout.cross_bin_dir.join(name);
            if let Some(qemu_runner) = &qemu_runner {
                write_guest_exec_wrapper(
                    &path,
                    qemu_runner,
                    &layout.staging_root,
                    &guest_relative_path,
                    None,
                )?;
            } else {
                let body = format!("exec {} \"$@\"\n", wrappers::shell_single_quote(&tool_path));
                wrappers::write_wrapper_script(&path, &body)?;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_wrappers_select_tools_and_preserve_process_contract() {
        let root = tempfile::tempdir().unwrap();
        let layout = case_assets::case_asset_layout(
            root.path(),
            "aarch64-unknown-none-softfloat",
            "cross tools' case",
        )
        .unwrap();
        let spec = cross_compile_spec("aarch64").unwrap();
        let host_tool = root.path().join("host tool's executable");
        wrappers::write_wrapper_script(&host_tool, "printf '%s\\n' \"$@\"\nexit 7\n").unwrap();

        // Native tools must not require guest binutils to be installed.
        write_cross_bin_wrappers_with_lookup(&layout, spec, |name| {
            name.starts_with(spec.gnu_tool_prefix)
                .then(|| host_tool.clone())
        })
        .unwrap();
        for name in ["ld", "aarch64-linux-musl-ld"] {
            let output = Command::new(layout.cross_bin_dir.join(name))
                .args(["space separated", "quote'argument", ""])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert_eq!(output.stdout, b"space separated\nquote'argument\n\n");
        }
        let error = write_cross_bin_wrappers_with_lookup(&layout, spec, |_| None).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required native cross tool `aarch64-linux-musl-ld`")
        );

        for tool in CROSS_BINUTILS {
            let path = layout.staging_root.join(spec.guest_tool_dir).join(tool);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"").unwrap();
        }
        // With both providers available, only qemu is selected. Record its
        // actual argv and environment, without depending on a host installation.
        wrappers::write_wrapper_script(
            &host_tool,
            "printf '%s\\n' \"$QEMU_LD_PREFIX\" \"$LD_LIBRARY_PATH\" \"$@\"\nexit 7\n",
        )
        .unwrap();
        write_cross_bin_wrappers_with_lookup(&layout, spec, |_| Some(host_tool.clone())).unwrap();
        let guest_ld = layout.staging_root.join(spec.guest_tool_dir).join("ld");
        for name in ["ld", "aarch64-linux-musl-ld"] {
            let output = Command::new(layout.cross_bin_dir.join(name))
                .arg("guest argument")
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(7));
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{}\n{}\n-0\n{}\n-L\n{}\n{}\nguest argument\n",
                    layout.staging_root.display(),
                    guest_library_path(&layout.staging_root),
                    guest_ld.display(),
                    layout.staging_root.display(),
                    guest_ld.display(),
                )
            );
        }
        fs::remove_file(guest_ld).unwrap();
        let error =
            write_cross_bin_wrappers_with_lookup(&layout, spec, |_| Some(host_tool.clone()))
                .unwrap_err();
        assert!(error.to_string().contains("missing required guest tool"));
    }
}
