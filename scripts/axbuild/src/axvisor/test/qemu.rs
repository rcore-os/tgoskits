use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, anyhow};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};
use test_case::TestQemuCase;

use super::{
    AXVISOR_NORMAL_GROUP, AxvisorQemuCase,
    assets::axvisor_case_asset_config,
    discover_qemu_cases,
    discovery::{
        discover_test_group_names, qemu_list_error_is_ignorable, test_suite_dir, test_suite_root,
    },
    initramfs::prepare_configured_busybox_initramfs,
    parse_target,
    types::PreparedAxvisorQemuCase,
};
use crate::{
    axvisor::{ArgsTestQemu, Axvisor, build, ovmf, rootfs},
    context::{AxvisorCliArgs, ResolvedAxvisorRequest, SnapshotPersistence},
    test::{case as test_case, qemu as test_qemu},
};

const VCPU_RUNTIME_ERROR: &str = r"VM\[\d+\] run VCpu\[\d+\] get error";

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
                            if qemu_list_error_is_ignorable(err.kind()) {
                                None
                            } else {
                                Some(Err(anyhow::Error::new(err)))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                anyhow::bail!(
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
            )
            .map_err(anyhow::Error::new)?;
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

        // Verify the firmware bundle before any build work so a bad bundle
        // fails fast. Runs after the `--list` early returns, so listing cases
        // never requires a firmware bundle. Without `--firmware-bundle-path`
        // the ovmf-entry case fails fast when its QEMU config is wired (instead
        // of hanging at `Booting from ROM..` or silently using a distro OVMF),
        // and other cases keep their exact previous behavior.
        //
        // Bundle verification intentionally happens after case discovery: only
        // discovery knows whether an ovmf-entry case is actually selected
        // (which cases exist depends on the arch/target/group), and the
        // discovery phase is cheap. The prepare phase then fails fast for the
        // ovmf-entry case before any build or QEMU work starts.
        let firmware_bundle = verify_cli_firmware_bundle(&args)?;

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
            .prepare_qemu_cases(&request, cases, firmware_bundle.as_ref())
            .await
            .context("failed to load Axvisor qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut summary = test_qemu::QemuTestSummary::default();
        let asset_config = axvisor_case_asset_config();

        let mut build_groups = test_qemu::prepare_case_build_groups(&cases, |build_config_path| {
            Self::qemu_group_build_context(&request, build_config_path, firmware_bundle.as_ref())
        })?;

        // Phase 1: Build all build groups first so compilation errors surface
        // before any QEMU time is spent.
        for build_group in &mut build_groups {
            rootfs::ensure_qemu_rootfs_ready(&build_group.request, self.app.workspace_root(), None)
                .await?;
            build_group.cargo = build::load_cargo_config(&build_group.request)?;
            prepare_configured_busybox_initramfs(
                &build_group.request,
                &build_group.cargo,
                self.app.workspace_root(),
            )
            .await?;
            self.app
                .build(
                    build_group.cargo.clone(),
                    build_group.request.build_info_path.clone(),
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to build Axvisor qemu test artifact for build group `{}` ({})",
                        build_group.group.build_group,
                        build_group.group.build_config_path.display()
                    )
                })?;
        }

        // Phase 2: Run all QEMU tests now that every artifact is available.
        let mut completed = 0;
        for build_group in &build_groups {
            for case in &build_group.group.cases {
                completed += 1;
                let case_name = &case.case.case.name;
                println!("[{completed}/{total}] axvisor qemu {case_name}");

                let case_started = Instant::now();
                let result = self
                    .run_qemu_case(
                        &build_group.request,
                        &build_group.cargo,
                        case,
                        &asset_config,
                    )
                    .await
                    .with_context(|| format!("axvisor qemu test failed for case `{case_name}`"));
                let duration = case_started.elapsed();
                match result {
                    Ok(()) => {
                        println!("ok: {case_name} ({duration:.2?})");
                        summary.pass_with_detail(case_name, format!("{duration:.2?}"));
                    }
                    Err(err) => {
                        eprintln!("failed: {}: {err:#}", case_name);
                        summary.fail_with_detail(case_name, format!("{duration:.2?}"));
                    }
                }
            }
        }

        let total_duration = format!("{:.2?}", suite_started.elapsed());
        summary.finish_with_total_detail("axvisor", "case", Some(total_duration.as_str()))
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cases: Vec<AxvisorQemuCase>,
        firmware_bundle: Option<&ovmf::VerifiedOvmfBundle>,
    ) -> anyhow::Result<Vec<PreparedAxvisorQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        let mut cargo_by_build_config = BTreeMap::new();
        for case in cases {
            let cargo = Self::qemu_case_cargo_config(
                request,
                &case.build_config_path,
                &mut cargo_by_build_config,
            )?;
            let mut qemu = self
                .app
                .read_qemu_config_from_path_for_cargo(&cargo, &case.case.qemu_config_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read Axvisor qemu config for case `{}`",
                        case.case.display_name
                    )
                })?;
            test_qemu::validate_grouped_qemu_commands(&qemu, &case.case, "Axvisor")?;
            wire_ovmf_entry_qemu_firmware(&mut qemu, &case.case, firmware_bundle)?;
            prepared.push(PreparedAxvisorQemuCase { case, qemu });
        }

        Ok(prepared)
    }

    fn qemu_case_cargo_config(
        request: &ResolvedAxvisorRequest,
        build_config_path: &Path,
        cargo_by_build_config: &mut BTreeMap<PathBuf, Cargo>,
    ) -> anyhow::Result<Cargo> {
        if let Some(cargo) = cargo_by_build_config.get(build_config_path) {
            return Ok(cargo.clone());
        }

        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        let cargo = build::load_cargo_config(&request)?;
        cargo_by_build_config.insert(build_config_path.to_path_buf(), cargo.clone());
        Ok(cargo)
    }

    fn qemu_group_build_context(
        request: &ResolvedAxvisorRequest,
        build_config_path: &Path,
        firmware_bundle: Option<&ovmf::VerifiedOvmfBundle>,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, Cargo)> {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        let cargo = build::load_cargo_config(&request)?;
        request.vmconfigs = build::vmconfigs_from_cargo(&cargo);
        let workspace_root = build::workspace_root_from_axvisor_dir(&request.axvisor_dir);
        request.vmconfigs = wire_ovmf_entry_vmconfig(
            request.vmconfigs,
            firmware_bundle,
            &workspace_root,
            &request.target,
        )?;

        Ok((request, cargo))
    }

    pub(super) fn qemu_test_request(mut request: ResolvedAxvisorRequest) -> ResolvedAxvisorRequest {
        request.smp = None;
        request.vmconfigs.clear();
        request
    }

    async fn load_qemu_case_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        case: &PreparedAxvisorQemuCase,
        asset_config: &test_case::CaseAssetConfig,
    ) -> anyhow::Result<(QemuConfig, test_case::PreparedCaseAssets)> {
        let mut qemu = case.qemu.clone();
        test_case::apply_grouped_qemu_config(
            &mut qemu,
            &case.case.case,
            &asset_config.grouped_runner,
        );
        test_qemu::apply_timeout_scale(&mut qemu);
        if !qemu
            .fail_regex
            .iter()
            .any(|pattern| pattern == VCPU_RUNTIME_ERROR)
        {
            qemu.fail_regex.push(VCPU_RUNTIME_ERROR.to_string());
        }

        let rootfs_path = rootfs::qemu_rootfs_path(request, self.app.workspace_root(), None)?;
        let prepared_assets = test_case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            &case.case.case,
            rootfs_path,
            asset_config.clone(),
        )
        .await?;
        rootfs::patch_qemu_rootfs_path(&mut qemu, &prepared_assets.rootfs_path);
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        // UEFI needs a writable ESP for firmware variables. Keep the explicit
        // snapshot isolation, but apply it per drive so QEMU does not make the
        // `fat:rw` ESP read-only through the global `-snapshot` flag.
        if qemu.uefi {
            test_qemu::apply_drive_snapshot_without_global_snapshot(&mut qemu);
        }
        Ok((qemu, prepared_assets))
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        case: &PreparedAxvisorQemuCase,
        asset_config: &test_case::CaseAssetConfig,
    ) -> anyhow::Result<()> {
        let prepare_started = Instant::now();
        let (qemu, prepared_assets) = self
            .load_qemu_case_config(request, case, asset_config)
            .await?;
        test_case::run_qemu_with_prepared_case_assets(
            &mut self.app,
            cargo,
            qemu,
            None,
            &case.case.case.qemu_config_path,
            prepared_assets,
            test_case::RunPreparedQemuCaseOptions {
                prepare_elapsed: prepare_started.elapsed(),
                qemu_timing_fields: None,
            },
        )
        .await
    }
}

/// VM config template file name that selects the fixed OVMF entry case.
const OVMF_ENTRY_VM_CONFIG_FILE: &str = "ovmf-entry.toml";

/// Case directory name (the test-suit `uefi` group directory holding the
/// ovmf-entry qemu configs) that selects the fixed OVMF entry case.
///
/// This is the file name of the case directory itself, not the discovered case
/// name: discovery names each case after its build wrapper (`ovmf-entry-vmx` /
/// `ovmf-entry-svm`), so the case name carries the variant suffix and can only
/// be matched by prefix.
const OVMF_ENTRY_CASE_DIR: &str = "ovmf-entry";

/// QEMU case name prefix (as discovered for the `uefi` group) that selects the
/// fixed OVMF entry case. The variant suffix (`-vmx` / `-svm`) comes from the
/// build wrapper, so the case is matched by the wrapper prefix.
///
/// The prefix match is deliberately narrow: it only accepts cases whose
/// directory is exactly `OVMF_ENTRY_CASE_DIR` (enforced by
/// [`ensure_ovmf_entry_case_dir`]), which is the same directory the checked-in
/// VM config template `OVMF_ENTRY_VM_CONFIG_FILE` lives in. A future
/// non-ovmf-entry case whose name merely starts with the same prefix is
/// rejected instead of being silently wired to the fixed bundle. A future
/// ovmf-entry variant that must NOT use the manifest-verified fixed CODE
/// bundle must re-evaluate this match so it cannot wire the wrong case.
const OVMF_ENTRY_QEMU_CASE_PREFIX: &str = "ovmf-entry-";

/// Wires the ovmf-entry QEMU case to the manifest-verified firmware bundle.
///
/// The QEMU layer boots the Axvisor host with `-kernel <axvisor.bin>`, where
/// the binary is a PE32+ UEFI image. QEMU can only load such an image through
/// its OVMF firmware, so without explicit pflash drives the run falls through
/// to SeaBIOS and hangs at `Booting from ROM..` (see the `ktest`
/// `patch_system_x86_64_uefi_kernel_loader` precedent, which documents the
/// same requirement). This function injects the verified bundle CODE as the
/// read-only pflash unit 0 and a writable copy of the bundle VARS as unit 1.
///
/// The injected CODE is the same file that `verify_firmware` accepted and that
/// the generated VM config embeds via `include_bytes!`, so the QEMU-layer
/// firmware and the nested OVMF loaded by Axvisor are byte-for-byte identical.
///
/// Without `--firmware-bundle-path` the ovmf-entry case cannot boot at all;
/// instead of hanging for `timeout` seconds (or silently using a distro OVMF),
/// the run fails fast with a clear error naming the required flag.
///
/// Only cases whose directory is exactly `OVMF_ENTRY_CASE_DIR` are wired (see
/// [`ensure_ovmf_entry_case_dir`]); other cases are never touched.
///
/// The unit 1 VARS file is a fresh copy created from the verified bundle
/// template on every run, so it never carries state between runs. It does not
/// rely on the `-snapshot` isolation that `qemu.uefi` cases get through
/// [`test_qemu::apply_drive_snapshot_without_global_snapshot`] (ovmf-entry
/// cases run with `uefi = false`). If a future case must let the guest write
/// VARS persistently and still stay isolated per run, the pflash unit 1 drive
/// needs the same per-drive snapshot handling as the `qemu.uefi` path.
fn wire_ovmf_entry_qemu_firmware(
    qemu: &mut QemuConfig,
    case: &TestQemuCase,
    firmware_bundle: Option<&ovmf::VerifiedOvmfBundle>,
) -> anyhow::Result<()> {
    if !case.name.starts_with(OVMF_ENTRY_QEMU_CASE_PREFIX) {
        return Ok(());
    }
    ensure_ovmf_entry_case_dir(case)?;
    let bundle = firmware_bundle.ok_or_else(|| {
        // `concat!` keeps the rendered message (three lines, one bullet per
        // line) visible in the source instead of relying on `\` line
        // continuations whose whitespace is stripped at runtime.
        anyhow!(
            concat!(
                "case `{name}` needs a verified OVMF firmware bundle:\n",
                "- pass `--firmware-bundle-path <bundle-dir>` for a managed bundle with ",
                "manifest.toml\n",
                "- `--allow-unverified-firmware` is only for a local CODE file and must not ",
                "determine UEFI test results",
            ),
            name = case.name,
        )
    })?;
    let vars_template = bundle
        .code_path
        .parent()
        .ok_or_else(|| {
            anyhow!(
                "verified OVMF CODE path {} has no parent directory",
                bundle.code_path.display()
            )
        })?
        .join(ovmf::VARS_FILE);
    if !vars_template.is_file() {
        anyhow::bail!(
            "missing {} next to the CODE file {} (required for the QEMU-layer pflash unit 1)",
            vars_template.display(),
            bundle.code_path.display()
        );
    }
    let vars = vars_template.with_extension("ovmf-entry.vars.fd");
    fs::copy(&vars_template, &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars from {} to {}",
            vars_template.display(),
            vars.display()
        )
    })?;
    qemu.args.extend([
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            bundle.code_path.display()
        ),
        "-drive".to_string(),
        format!("if=pflash,format=raw,unit=1,file={}", vars.display()),
    ]);
    println!(
        "wired verified OVMF firmware {} into QEMU layer for case `{}`",
        bundle.code_path.display(),
        case.name
    );
    Ok(())
}

/// Verifies that a prefix-matched case really lives in the ovmf-entry case
/// directory before the fixed firmware bundle is wired to it.
///
/// The case name (`ovmf-entry-<variant>`) alone is a weak selector: discovery
/// names every case after its build wrapper, so any future case whose
/// directory starts with the same prefix would be caught by the match. The
/// case directory is the second, exact selector: only a case whose directory
/// is named exactly `OVMF_ENTRY_CASE_DIR` (the directory that also holds the
/// `OVMF_ENTRY_VM_CONFIG_FILE` template) may use the manifest-verified fixed
/// CODE bundle.
fn ensure_ovmf_entry_case_dir(case: &TestQemuCase) -> anyhow::Result<()> {
    let dir_name = case
        .case_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if dir_name == OVMF_ENTRY_CASE_DIR {
        return Ok(());
    }
    anyhow::bail!(
        "case `{}` matches the ovmf-entry prefix but is not in the ovmf-entry case directory \
         (directory `{}` is not `{OVMF_ENTRY_CASE_DIR}`); add the case to the fixed ovmf-entry \
         bundle or rename it so it does not use the prefix",
        case.name,
        case.case_dir.display()
    )
}

/// Rewires the ovmf-entry VM config to the manifest-verified firmware bundle.
///
/// When `--firmware-bundle-path` is supplied, every build group whose VM
/// configs include the checked-in `ovmf-entry.toml` template gets a generated
/// per-run config with `image_location = "memory"` and `kernel_path` /
/// `uefi_firmware_path` pointing at the verified CODE file. `os/axvisor/
/// build.rs` then embeds that exact CODE into the Axvisor binary with
/// `include_bytes!`, so the nested OVMF loaded by Axvisor is byte-for-byte
/// the firmware that `verify_firmware` accepted.
///
/// Groups that do not use the template (or runs without a bundle) keep their
/// vm_configs untouched.
fn wire_ovmf_entry_vmconfig(
    vmconfigs: Vec<PathBuf>,
    firmware_bundle: Option<&ovmf::VerifiedOvmfBundle>,
    workspace_root: &Path,
    target: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let Some(bundle) = firmware_bundle else {
        return Ok(vmconfigs);
    };
    let Some(index) = vmconfigs.iter().position(|path| {
        path.file_name()
            .is_some_and(|name| name == OVMF_ENTRY_VM_CONFIG_FILE)
    }) else {
        return Ok(vmconfigs);
    };

    let template_path = &vmconfigs[index];
    let generated = generate_ovmf_entry_vm_config(template_path, bundle, workspace_root, target)?;
    let mut rewired = vmconfigs;
    rewired[index] = generated;
    Ok(rewired)
}

/// Writes the per-run ovmf-entry VM config embedding the verified CODE.
///
/// The generated config keeps the checked-in template's `[base]`, memory
/// regions, and device selection, and overrides only the kernel image source:
/// `image_location = "memory"` with absolute `kernel_path` /
/// `uefi_firmware_path` pointing at the verified CODE file. The output is a
/// real file on disk because `os/axvisor/build.rs` resolves the paths relative
/// to the config file location at build time.
fn generate_ovmf_entry_vm_config(
    template_path: &Path,
    bundle: &ovmf::VerifiedOvmfBundle,
    workspace_root: &Path,
    target: &str,
) -> anyhow::Result<PathBuf> {
    let template = fs::read_to_string(template_path).with_context(|| {
        format!(
            "failed to read ovmf-entry VM config template {}",
            template_path.display()
        )
    })?;
    let mut config: toml::Value = toml::from_str(&template)
        .with_context(|| format!("failed to parse {}", template_path.display()))?;
    let kernel = config
        .get_mut("kernel")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            anyhow!(
                "ovmf-entry VM config {} has no [kernel] table",
                template_path.display()
            )
        })?;
    let code_path = bundle.code_path.display().to_string();
    kernel.insert(
        "image_location".to_string(),
        toml::Value::String("memory".to_string()),
    );
    kernel.insert(
        "kernel_path".to_string(),
        toml::Value::String(code_path.clone()),
    );
    kernel.insert(
        "uefi_firmware_path".to_string(),
        toml::Value::String(code_path),
    );
    kernel.insert(
        "bios_load_addr".to_string(),
        toml::Value::Integer(ovmf::OVMF_CODE_BASE as i64),
    );
    kernel.insert(
        "firmware_profile".to_string(),
        toml::Value::String(ovmf::OVMF_PROFILE_NAME.to_string()),
    );
    let output = toml::to_string_pretty(&config)
        .with_context(|| format!("failed to serialize {}", template_path.display()))?;

    let output_path = ovmf_entry_generated_config_path(workspace_root, target);
    let parent = output_path.parent().with_context(|| {
        format!(
            "generated ovmf-entry VM config has no parent: {}",
            output_path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    fs::write(&output_path, output)
        .with_context(|| format!("failed to write {}", output_path.display()))?;
    println!(
        "wired verified OVMF CODE {} into {}",
        bundle.code_path.display(),
        output_path.display()
    );
    Ok(output_path)
}

/// Computes the per-run generated VM config path for an ovmf-entry template.
///
/// The path lives under the build target's `qemu-cases` work directory, next
/// to the case asset layout used by the other QEMU test cases.
///
/// Both ovmf-entry variants (`ovmf-entry-vmx` and `ovmf-entry-svm`) share this
/// single path: they use the same target (`x86_64-unknown-none`) and the same
/// verified bundle, so the generated content is identical and the last write
/// wins harmlessly. If a future variant diverges in target or firmware, the
/// path must become variant-scoped.
fn ovmf_entry_generated_config_path(workspace_root: &Path, target: &str) -> PathBuf {
    workspace_root
        .join("target")
        .join(target)
        .join("qemu-cases")
        .join("ovmf-entry")
        .join(format!("{OVMF_ENTRY_VM_CONFIG_FILE}.vmconfig.toml"))
}

/// Verifies the CLI-selected firmware bundle, if any.
///
/// Without `--firmware-bundle-path` no firmware is resolved and `None` is
/// returned; existing test runs keep their exact previous behavior. With a
/// bundle path the source is resolved and fully verified, and any failure
/// aborts the run with a clear error before any build or QEMU work starts.
fn verify_cli_firmware_bundle(
    args: &ArgsTestQemu,
) -> anyhow::Result<Option<ovmf::VerifiedOvmfBundle>> {
    let Some(path) = args.firmware_bundle_path.as_deref() else {
        return Ok(None);
    };
    let source =
        ovmf::FirmwareSource::from_cli(Some(path.to_path_buf()), args.allow_unverified_firmware)
            .with_context(|| format!("failed to resolve OVMF firmware path `{}`", path.display()))?
            .ok_or_else(|| {
                anyhow!(
                    "OVMF firmware path `{}` did not resolve to a firmware source",
                    path.display()
                )
            })?;
    let bundle = ovmf::verify_firmware(&source)
        .with_context(|| format!("failed to verify OVMF firmware `{}`", path.display()))?;
    println!(
        "verified OVMF firmware bundle for ovmf-entry cases: {} (sha256={})",
        bundle.code_path.display(),
        bundle.code_sha256
    );
    Ok(Some(bundle))
}

fn axvisor_qemu_test_build_args(arch: &str, config: Option<PathBuf>) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config,
        arch: Some(arch.to_string()),
        target: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    const TEST_OVMF_ENTRY_TEMPLATE: &str = r#"
[base]
id = 1
name = "ovmf-entry"
guest_type = "passthrough"
cpu_num = 1
phys_cpu_sets = [1]

[kernel]
entry_point = 0xffff_fff0
image_location = "fs"
kernel_path = "/guest/ovmf/OVMF_CODE.fd"
kernel_load_addr = 0x20_0000
enable_bios = true
boot_protocol = "uefi"
uefi_firmware_path = "/guest/ovmf/OVMF_CODE.fd"
bios_load_addr = 0xffc8_4000
firmware_profile = "qemu_x86_64_axvisor_ovmf_debug"

memory_regions = [
  [0x0000_0000, 0x100_0000, 0x7, 0],
  [0xffc0_0000, 0x40_0000, 0x7, 0],
]

[devices]
passthrough = []
disabled = []
"#;

    fn write_template(root: &Path) -> PathBuf {
        let path = root.join("os/axvisor/configs/vms/qemu/x86_64/ovmf-entry.toml");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, TEST_OVMF_ENTRY_TEMPLATE).unwrap();
        path
    }

    fn fixture_bundle(code_path: &Path) -> ovmf::VerifiedOvmfBundle {
        ovmf::VerifiedOvmfBundle {
            profile: ovmf::OVMF_PROFILE_NAME.to_string(),
            code_base: ovmf::OVMF_CODE_BASE,
            code_size: ovmf::OVMF_CODE_SIZE,
            vars_base: ovmf::OVMF_VARS_BASE,
            vars_size: ovmf::OVMF_VARS_SIZE,
            combined_size: ovmf::OVMF_COMBINED_SIZE,
            code_path: code_path.to_path_buf(),
            code_sha256: "ab".repeat(32),
            verified: true,
        }
    }

    #[test]
    fn wiring_replaces_ovmf_entry_template_with_generated_memory_config() {
        let root = tempdir().unwrap();
        let template = write_template(root.path());
        let code = root.path().join("bundle/OVMF_CODE.fd");
        let bundle = fixture_bundle(&code);
        let vmconfigs = vec![template.clone()];

        let rewired =
            wire_ovmf_entry_vmconfig(vmconfigs, Some(&bundle), root.path(), "x86_64-unknown-none")
                .unwrap();

        assert_eq!(rewired.len(), 1);
        assert_ne!(rewired[0], template);
        assert!(
            rewired[0].starts_with(
                root.path()
                    .join("target/x86_64-unknown-none/qemu-cases/ovmf-entry")
            )
        );

        let generated = fs::read_to_string(&rewired[0]).unwrap();
        let config: toml::Value = toml::from_str(&generated).unwrap();
        let kernel = config.get("kernel").unwrap().as_table().unwrap();
        assert_eq!(
            kernel.get("image_location").unwrap().as_str(),
            Some("memory")
        );
        assert_eq!(
            kernel.get("kernel_path").unwrap().as_str(),
            Some(code.to_str().unwrap())
        );
        assert_eq!(
            kernel.get("uefi_firmware_path").unwrap().as_str(),
            Some(code.to_str().unwrap())
        );
        assert_eq!(
            kernel.get("bios_load_addr").unwrap().as_integer(),
            Some(ovmf::OVMF_CODE_BASE as i64)
        );
        assert_eq!(
            kernel.get("firmware_profile").unwrap().as_str(),
            Some(ovmf::OVMF_PROFILE_NAME)
        );
        // The generated config must keep the checked-in memory regions.
        let regions = config
            .get("kernel")
            .unwrap()
            .get("memory_regions")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!(
            regions[1].as_array().unwrap()[0].as_integer(),
            Some(0xffc0_0000)
        );
    }

    #[test]
    fn wiring_keeps_other_vm_configs_untouched() {
        let root = tempdir().unwrap();
        write_template(root.path());
        let other = root
            .path()
            .join("os/axvisor/configs/vms/qemu/x86_64/arceos-smp1.toml");
        fs::write(&other, "[base]\nid = 2\n").unwrap();
        let bundle = fixture_bundle(&root.path().join("bundle/OVMF_CODE.fd"));

        let rewired = wire_ovmf_entry_vmconfig(
            vec![other.clone()],
            Some(&bundle),
            root.path(),
            "x86_64-unknown-none",
        )
        .unwrap();

        assert_eq!(rewired, vec![other]);
    }

    #[test]
    fn wiring_without_bundle_keeps_vmconfigs_untouched() {
        let root = tempdir().unwrap();
        let template = write_template(root.path());

        let rewired = wire_ovmf_entry_vmconfig(
            vec![template.clone()],
            None,
            root.path(),
            "x86_64-unknown-none",
        )
        .unwrap();

        assert_eq!(rewired, vec![template]);
    }

    #[test]
    fn generated_config_parses_as_axvmconfig_guest() {
        let root = tempdir().unwrap();
        let template = write_template(root.path());
        let code = root.path().join("bundle/OVMF_CODE.fd");
        let bundle = fixture_bundle(&code);
        let generated =
            generate_ovmf_entry_vm_config(&template, &bundle, root.path(), "x86_64-unknown-none")
                .unwrap();
        let content = fs::read_to_string(&generated).unwrap();

        let config = axvmconfig::GuestConfig::from_toml(&content).unwrap();
        config.kernel.validate_boot_config().unwrap();
        assert_eq!(config.kernel.image_location.as_deref(), Some("memory"));
        assert_eq!(
            config.kernel.firmware_profile.as_deref(),
            Some(ovmf::OVMF_PROFILE_NAME)
        );
        assert_eq!(
            config.kernel.bios_load_addr,
            Some(ovmf::OVMF_CODE_BASE as usize)
        );
        assert_eq!(config.kernel.entry_point, ovmf::OVMF_RESET_VECTOR as usize);
    }

    fn test_qemu_args(
        arch: Option<String>,
        firmware_bundle_path: Option<PathBuf>,
        allow_unverified_firmware: bool,
    ) -> ArgsTestQemu {
        ArgsTestQemu {
            arch,
            target: None,
            test_group: None,
            test_case: None,
            list: false,
            firmware_bundle_path,
            allow_unverified_firmware,
        }
    }

    #[test]
    fn verify_cli_firmware_bundle_returns_none_without_path() {
        let args = test_qemu_args(Some("x86_64".to_string()), None, false);

        assert!(verify_cli_firmware_bundle(&args).unwrap().is_none());
    }

    #[test]
    fn verify_cli_firmware_bundle_rejects_missing_bundle() {
        let root = tempdir().unwrap();
        let missing = root.path().join("missing-bundle");
        let args = test_qemu_args(Some("x86_64".to_string()), Some(missing.clone()), false);

        let err = verify_cli_firmware_bundle(&args).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(rendered.contains("does not exist"), "{rendered}");
    }

    #[test]
    fn ovmf_entry_generated_config_path_is_target_scoped() {
        let path = ovmf_entry_generated_config_path(Path::new("/ws"), "x86_64-unknown-none");

        assert_eq!(
            path,
            PathBuf::from(
                "/ws/target/x86_64-unknown-none/qemu-cases/ovmf-entry/ovmf-entry.toml.vmconfig.\
                 toml"
            )
        );
    }

    fn fixture_qemu_case(name: &str) -> TestQemuCase {
        TestQemuCase {
            name: name.to_string(),
            display_name: name.to_string(),
            case_dir: PathBuf::from("/ws/test-suit/axvisor/uefi/ovmf-entry"),
            qemu_config_path: PathBuf::from("/ws/qemu-x86_64-vmx.toml"),
            test_commands: Vec::new(),
            host_symbolize_success_regex: Vec::new(),
            host_http_server: None,
            subcases: Vec::new(),
            grouped_subcase_filter: None,
        }
    }

    #[test]
    fn qemu_firmware_wiring_injects_verified_bundle_pflash_for_ovmf_entry() {
        let root = tempdir().unwrap();
        let bundle_dir = root.path().join("bundle");
        fs::create_dir_all(&bundle_dir).unwrap();
        let code = bundle_dir.join("OVMF_CODE.fd");
        fs::write(&code, vec![0xa5; ovmf::OVMF_CODE_SIZE as usize]).unwrap();
        let vars = bundle_dir.join(ovmf::VARS_FILE);
        fs::write(&vars, vec![0x5a; ovmf::OVMF_VARS_SIZE as usize]).unwrap();
        let bundle = fixture_bundle(&code);

        let mut qemu = QemuConfig {
            args: vec!["-kernel".to_string(), "axvisor.bin".to_string()],
            uefi: false,
            to_bin: true,
            success_regex: Vec::new(),
            fail_regex: Vec::new(),
            shell_prefix: None,
            shell_init_cmd: None,
            timeout: Some(300),
        };
        wire_ovmf_entry_qemu_firmware(
            &mut qemu,
            &fixture_qemu_case("ovmf-entry-vmx"),
            Some(&bundle),
        )
        .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-kernel".to_string(),
                "axvisor.bin".to_string(),
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    code.display()
                ),
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,unit=1,file={}",
                    bundle_dir.join("OVMF_VARS.ovmf-entry.vars.fd").display()
                ),
            ]
        );
        // The writable VARS copy must exist next to the template.
        assert!(bundle_dir.join("OVMF_VARS.ovmf-entry.vars.fd").is_file());
    }

    #[test]
    fn qemu_firmware_wiring_leaves_other_cases_untouched() {
        let root = tempdir().unwrap();
        let bundle = fixture_bundle(&root.path().join("bundle/OVMF_CODE.fd"));

        let mut qemu = QemuConfig {
            args: vec!["-kernel".to_string(), "axvisor.bin".to_string()],
            uefi: false,
            to_bin: true,
            success_regex: Vec::new(),
            fail_regex: Vec::new(),
            shell_prefix: None,
            shell_init_cmd: None,
            timeout: Some(300),
        };
        let original = qemu.args.clone();
        wire_ovmf_entry_qemu_firmware(&mut qemu, &fixture_qemu_case("smoke-vmx"), Some(&bundle))
            .unwrap();

        assert_eq!(qemu.args, original);
    }

    #[test]
    fn qemu_firmware_wiring_without_bundle_fails_fast_for_ovmf_entry() {
        let mut qemu = QemuConfig {
            args: vec!["-kernel".to_string(), "axvisor.bin".to_string()],
            uefi: false,
            to_bin: true,
            success_regex: Vec::new(),
            fail_regex: Vec::new(),
            shell_prefix: None,
            shell_init_cmd: None,
            timeout: Some(300),
        };

        let err =
            wire_ovmf_entry_qemu_firmware(&mut qemu, &fixture_qemu_case("ovmf-entry-svm"), None)
                .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("--firmware-bundle-path"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn qemu_firmware_wiring_rejects_bundle_without_vars() {
        let root = tempdir().unwrap();
        let bundle_dir = root.path().join("bundle");
        fs::create_dir_all(&bundle_dir).unwrap();
        let code = bundle_dir.join("OVMF_CODE.fd");
        fs::write(&code, vec![0xa5; ovmf::OVMF_CODE_SIZE as usize]).unwrap();
        let bundle = fixture_bundle(&code);

        let mut qemu = QemuConfig {
            args: Vec::new(),
            uefi: false,
            to_bin: true,
            success_regex: Vec::new(),
            fail_regex: Vec::new(),
            shell_prefix: None,
            shell_init_cmd: None,
            timeout: Some(300),
        };
        let err = wire_ovmf_entry_qemu_firmware(
            &mut qemu,
            &fixture_qemu_case("ovmf-entry-vmx"),
            Some(&bundle),
        )
        .unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("OVMF_VARS.fd"),
            "unexpected error: {rendered}"
        );
    }

    #[test]
    fn qemu_firmware_wiring_rejects_prefix_match_outside_ovmf_entry_dir() {
        let mut qemu = QemuConfig {
            args: Vec::new(),
            uefi: false,
            to_bin: true,
            success_regex: Vec::new(),
            fail_regex: Vec::new(),
            shell_prefix: None,
            shell_init_cmd: None,
            timeout: Some(300),
        };
        let mut case = fixture_qemu_case("ovmf-entry-mem");
        case.case_dir = PathBuf::from("/ws/test-suit/axvisor/uefi/ovmf-entry-mem");
        let bundle = fixture_bundle(&PathBuf::from("/ws/bundle/OVMF_CODE.fd"));

        let err = wire_ovmf_entry_qemu_firmware(&mut qemu, &case, Some(&bundle)).unwrap_err();
        let rendered = format!("{err:#}");
        assert!(
            rendered.contains("not in the ovmf-entry case directory"),
            "unexpected error: {rendered}"
        );
        assert!(qemu.args.is_empty(), "no pflash args may be injected");
    }

    #[test]
    fn ensure_ovmf_entry_case_dir_accepts_only_the_ovmf_entry_directory() {
        assert!(ensure_ovmf_entry_case_dir(&fixture_qemu_case("ovmf-entry-vmx")).is_ok());
        assert!(ensure_ovmf_entry_case_dir(&fixture_qemu_case("ovmf-entry-svm")).is_ok());

        let mut other = fixture_qemu_case("ovmf-entry-mem");
        other.case_dir = PathBuf::from("/ws/test-suit/axvisor/uefi/ovmf-entry-mem");
        let err = ensure_ovmf_entry_case_dir(&other).unwrap_err();
        assert!(
            format!("{err:#}").contains("ovmf-entry"),
            "unexpected error: {err:#}"
        );

        // A non-prefixed name in an unrelated directory stays rejected.
        let mut smoke = fixture_qemu_case("smoke-vmx");
        smoke.case_dir = PathBuf::from("/ws/test-suit/axvisor/normal/qemu");
        assert!(ensure_ovmf_entry_case_dir(&smoke).is_err());
    }
}
