use std::collections::HashMap;

use anyhow::Context;
use cargo_metadata::{Metadata, Package};

use super::{AXSTD_STD_CLIPPY_TARGET, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE};

pub(super) fn clippy_env(_package: &Package) -> Vec<(String, String)> {
    Vec::new()
}

fn axstd_std_clippy_env(metadata: &Metadata) -> anyhow::Result<Vec<(String, String)>> {
    let mut envs = HashMap::new();
    crate::build::prepare_std_build_env(&mut envs, AXSTD_STD_CLIPPY_TARGET, metadata)
        .context("failed to prepare ax-std std clippy config")?;
    Ok(envs.into_iter().collect())
}

pub(super) fn feature_clippy_env(
    package: &Package,
    feature: &str,
    base_env: Vec<(String, String)>,
    metadata: &Metadata,
) -> anyhow::Result<Vec<(String, String)>> {
    if package.name == AXSTD_STD_PACKAGE && feature == AXSTD_STD_DEFAULT_FEATURE {
        return axstd_std_clippy_env(metadata);
    }

    Ok(base_env)
}
