use std::path::PathBuf;

use ostool::build::config::Cargo;

use crate::context::{
    AppContext, QemuRunConfig, ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotPersistence {
    Discard,
    Store,
}

pub(crate) trait CommandRequest {
    fn build_info_path(&self) -> PathBuf;
    fn uboot_config(&self) -> Option<PathBuf>;
    fn debug(&self) -> bool;
}

impl CommandRequest for ResolvedBuildRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn uboot_config(&self) -> Option<PathBuf> {
        self.uboot_config.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
}

impl CommandRequest for ResolvedStarryRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn uboot_config(&self) -> Option<PathBuf> {
        self.uboot_config.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
}

impl CommandRequest for ResolvedAxvisorRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn uboot_config(&self) -> Option<PathBuf> {
        self.uboot_config.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
}

pub(crate) fn resolve_request<R, S, Prepare, Store>(
    persistence: SnapshotPersistence,
    prepare: Prepare,
    store: Store,
) -> anyhow::Result<R>
where
    Prepare: FnOnce() -> anyhow::Result<(R, S)>,
    Store: FnOnce(&S) -> anyhow::Result<PathBuf>,
{
    let (request, snapshot) = prepare()?;
    if matches!(persistence, SnapshotPersistence::Store) {
        store(&snapshot)?;
    }
    Ok(request)
}

pub(crate) async fn run_build<R, LoadCargo>(
    app: &mut AppContext,
    request: R,
    load_cargo: LoadCargo,
) -> anyhow::Result<()>
where
    R: CommandRequest,
    LoadCargo: FnOnce(&R) -> anyhow::Result<Cargo>,
{
    app.set_debug_mode(request.debug())?;
    let cargo = load_cargo(&request)?;
    app.build(cargo, request.build_info_path()).await
}

pub(crate) async fn run_qemu<R, LoadCargo, LoadQemu>(
    app: &mut AppContext,
    request: R,
    load_cargo: LoadCargo,
    load_qemu: LoadQemu,
) -> anyhow::Result<()>
where
    R: CommandRequest,
    LoadCargo: FnOnce(&R) -> anyhow::Result<Cargo>,
    LoadQemu: FnOnce(&R) -> anyhow::Result<QemuRunConfig>,
{
    app.set_debug_mode(request.debug())?;
    let cargo = load_cargo(&request)?;
    let qemu = load_qemu(&request)?;
    app.qemu(cargo, request.build_info_path(), qemu).await
}

pub(crate) async fn run_uboot<R, LoadCargo>(
    app: &mut AppContext,
    request: R,
    load_cargo: LoadCargo,
) -> anyhow::Result<()>
where
    R: CommandRequest,
    LoadCargo: FnOnce(&R) -> anyhow::Result<Cargo>,
{
    app.set_debug_mode(request.debug())?;
    let cargo = load_cargo(&request)?;
    app.uboot(cargo, request.build_info_path(), request.uboot_config())
        .await
}
