//! Axloader-specific host checks and persistent-image OTA scenarios.
use anyhow::bail;

use crate::{
    axloader::{
        AXLOADER_PACKAGE, ArgsTestQemu, DEFAULT_UEFI_TARGET, LOONGARCH64_UEFI_TARGET, run_cargo,
        run_loader_build, run_loader_check,
    },
    support::ovmf::OvmfFirmware,
};
mod ota_qemu;

pub(crate) async fn test_qemu(
    workspace: &crate::context::WorkspaceContext,
    args: ArgsTestQemu,
) -> anyhow::Result<()> {
    run_cargo(
        workspace.root(),
        workspace.target_dir(),
        ["test", "-p", AXLOADER_PACKAGE, "--all-targets"],
    )?;
    run_loader_check(workspace.root(), workspace.target_dir(), &args.target)?;

    run_loader_build(
        workspace.root(),
        workspace.target_dir(),
        &args.target,
        true,
        true,
    )?;
    let firmware = match (args.firmware_code, args.firmware_vars) {
        (Some(code), Some(vars)) => Some(OvmfFirmware::from_paths(code, vars)?),
        (None, None) => None,
        _ => bail!("--firmware-code and --firmware-vars must be supplied together"),
    };
    ota_qemu::test_direct_ota(workspace, &args.target, args.server_only, firmware).await
}
