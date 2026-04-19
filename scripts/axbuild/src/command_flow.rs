use std::path::PathBuf;

use ostool::build::config::Cargo;

use crate::context::{
    AppContext, ResolvedAxvisorRequest, ResolvedBuildRequest, ResolvedStarryRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotPersistence {
    Discard,
    Store,
}

pub(crate) trait CommandRequest {
    fn build_info_path(&self) -> PathBuf;
    fn debug(&self) -> bool;
}

impl CommandRequest for ResolvedBuildRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
}

impl CommandRequest for ResolvedStarryRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
}

impl CommandRequest for ResolvedAxvisorRequest {
    fn build_info_path(&self) -> PathBuf {
        self.build_info_path.clone()
    }

    fn debug(&self) -> bool {
        self.debug
    }
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
