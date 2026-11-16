use super::ArceosConfig;
use crate::{build, context::ResolvedBuildRequest};

pub(super) fn config_with_makefile_features(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
) -> anyhow::Result<ArceosConfig> {
    let mut config = request.config.clone();
    build::apply_makefile_features(&mut config.common, makefile_features)?;
    config.validate()?;
    build::validate_features(&config.common)?;
    Ok(config)
}
