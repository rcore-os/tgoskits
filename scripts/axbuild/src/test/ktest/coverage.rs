use super::*;

pub(super) fn generate_ktest_coverage_report(
    out_fmt: KtestCoverageOutFmt,
    workspace_root: &Path,
    cargo: &Cargo,
    elf_path: &Path,
) -> anyhow::Result<()> {
    match out_fmt {
        KtestCoverageOutFmt::Html => generate_ktest_coverage_html(workspace_root, cargo, elf_path),
    }
}

pub(super) fn generate_ktest_coverage_html(
    workspace_root: &Path,
    cargo: &Cargo,
    elf_path: &Path,
) -> anyhow::Result<()> {
    let paths = crate::support::axtest_coverage::AxtestCoveragePaths::new(
        workspace_root,
        &cargo.package,
        cargo
            .test
            .as_deref()
            .context("axtest coverage report requires a Cargo test target")?,
        &cargo.target,
    )?;
    let profraw_path = paths.profraw_path;
    if !profraw_path.is_file() {
        bail!(
            "coverage profile was not found at {}; run ktest qemu with --coverage first",
            profraw_path.display()
        );
    }
    if !elf_path.is_file() {
        bail!("coverage binary was not found at {}", elf_path.display());
    }

    let profdata_path = profraw_path.with_extension("profdata");
    let stem = profraw_path
        .file_stem()
        .ok_or_else(|| anyhow!("invalid coverage profile path {}", profraw_path.display()))?
        .to_string_lossy();
    let html_dir = profraw_path.with_file_name(format!("{stem}-html"));
    if html_dir.exists() {
        fs::remove_dir_all(&html_dir)
            .with_context(|| format!("failed to remove {}", html_dir.display()))?;
    }

    let llvm_profdata = find_llvm_tool("llvm-profdata");
    let llvm_cov = find_llvm_tool("llvm-cov");
    run_tool(
        &llvm_profdata,
        [
            OsString::from("merge"),
            OsString::from("-sparse"),
            profraw_path.as_os_str().to_os_string(),
            OsString::from("-o"),
            profdata_path.as_os_str().to_os_string(),
        ],
    )
    .with_context(|| format!("failed to create {}", profdata_path.display()))?;
    run_tool(
        &llvm_cov,
        llvm_cov_html_args(elf_path, &profdata_path, &html_dir),
    )
    .with_context(|| {
        format!(
            "failed to create HTML coverage report in {}",
            html_dir.display()
        )
    })?;

    println!("  coverage profdata: {}", profdata_path.display());
    println!("  coverage html: {}/index.html", html_dir.display());
    Ok(())
}

pub(super) fn llvm_cov_html_args(
    elf_path: &Path,
    profdata_path: &Path,
    html_dir: &Path,
) -> Vec<OsString> {
    vec![
        OsString::from("show"),
        elf_path.as_os_str().to_os_string(),
        OsString::from(format!("-instr-profile={}", profdata_path.display())),
        OsString::from("-format=html"),
        OsString::from(format!("-output-dir={}", html_dir.display())),
        OsString::from(format!(
            "-ignore-filename-regex={COVERAGE_IGNORED_SOURCE_REGEX}"
        )),
    ]
}

pub(super) fn find_llvm_tool(tool: &str) -> PathBuf {
    if let Ok(output) = ProcessCommand::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        && output.status.success()
    {
        let sysroot = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let rustlib = Path::new(&sysroot).join("lib/rustlib");
        if let Ok(entries) = fs::read_dir(rustlib) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join(tool);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    PathBuf::from(tool)
}

pub(super) fn run_tool<I>(tool: &Path, args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = OsString>,
{
    let status = ProcessCommand::new(tool)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {}", tool.display()))?;
    if !status.success() {
        bail!("{} failed with status {status}", tool.display());
    }
    Ok(())
}
