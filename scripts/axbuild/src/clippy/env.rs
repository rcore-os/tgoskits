use cargo_metadata::{Metadata, Package};

use super::{AXSTD_STD_CLIPPY_TARGET, AXSTD_STD_DEFAULT_FEATURE, AXSTD_STD_PACKAGE};

pub(super) fn clippy_env(_package: &Package) -> Vec<(String, String)> {
    Vec::new()
}

pub(super) fn feature_clippy_env(
    package: &Package,
    feature: &str,
    base_env: Vec<(String, String)>,
    _metadata: &Metadata,
) -> anyhow::Result<Vec<(String, String)>> {
    if package.name == AXSTD_STD_PACKAGE && feature == AXSTD_STD_DEFAULT_FEATURE {
        // Clippy for the std-only ax-std target needs the original target
        // name; no feature or platform is inferred here.
        return Ok(vec![(
            "AX_TARGET".to_string(),
            AXSTD_STD_CLIPPY_TARGET.to_string(),
        )]);
    }

    Ok(base_env)
}
