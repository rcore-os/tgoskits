use std::path::Path;

use anyhow::Result;
use env_logger::Env;

pub(crate) fn init_logging(_workspace_root: &Path) -> Result<()> {
    let _ = env_logger::Builder::from_env(Env::default().default_filter_or("info")).try_init();
    Ok(())
}
