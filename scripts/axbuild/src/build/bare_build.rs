use std::{collections::HashMap, path::Path};

/// Resolved Cargo inputs for a logical freestanding target.
pub(crate) struct BareBuildTarget {
    pub(crate) target: String,
    pub(crate) cargo_args: Vec<String>,
    pub(crate) env: HashMap<String, String>,
}

/// Resolves target specifications shared by freestanding builds and Clippy.
pub(crate) fn bare_build_target_for(target: &str) -> BareBuildTarget {
    BareBuildTarget {
        target: Path::new("scripts/targets/bare")
            .join(format!("{target}.json"))
            .display()
            .to_string(),
        cargo_args: vec![
            "-Z".to_string(),
            "json-target-spec".to_string(),
            "-Z".to_string(),
            "build-std=core,alloc".to_string(),
        ],
        env: HashMap::from([(
            "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
            "true".to_string(),
        )]),
    }
}
