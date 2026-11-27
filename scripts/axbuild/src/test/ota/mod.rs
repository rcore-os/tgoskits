//! Axloader-specific host checks and persistent-image OTA scenarios.
use anyhow::bail;

use crate::{
    axloader::{
        AXLOADER_PACKAGE, ArgsTestQemu, DEFAULT_UEFI_TARGET, LOONGARCH64_UEFI_TARGET, run_cargo,
        run_loader_build, run_loader_check,
    },
    build::RISCV64_UEFI_TARGET,
    support::ovmf::OvmfFirmware,
};
mod ota_qemu;

pub(crate) fn test_host(workspace: &crate::context::WorkspaceContext) -> anyhow::Result<()> {
    run_cargo(
        workspace.root(),
        workspace.target_dir(),
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )
}

pub(crate) async fn test_qemu(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTestQemu,
) -> anyhow::Result<()> {
    test_host(workspace)?;
    run_loader_check(workspace.root(), workspace.target_dir(), &args.target)?;

    let loader_target_dir = if args.target == RISCV64_UEFI_TARGET {
        // Keep the fixture launcher out of the normal release artifact path.
        workspace.axbuild_artifact_dir().join("axloader/qemu-build")
    } else {
        workspace.target_dir().to_path_buf()
    };
    run_loader_build(
        workspace.root(),
        &loader_target_dir,
        &args.target,
        true,
        true,
    )?;
    let firmware = match (args.firmware_code, args.firmware_vars) {
        (Some(code), Some(vars)) => Some(OvmfFirmware::from_paths(code, vars)?),
        (None, None) => None,
        _ => bail!("--firmware-code and --firmware-vars must be supplied together"),
    };
    ota_qemu::test_direct_ota(
        workspace,
        &loader_target_dir,
        &args.target,
        args.server_only,
        firmware,
    )
    .await
}
