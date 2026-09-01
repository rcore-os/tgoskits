use std::{collections::HashMap, path::Path};

use crate::context::arch_spec_for_target;

/// Resolved Cargo inputs for a logical freestanding target.
pub(crate) struct BareBuildTarget {
    pub(crate) target: String,
    pub(crate) cargo_args: Vec<String>,
    pub(crate) env: HashMap<String, String>,
}

/// Resolves one of the four workspace-owned freestanding target specifications.
pub(crate) fn bare_build_target_for(target: &str) -> Option<BareBuildTarget> {
    arch_spec_for_target(target)?;
    Some(BareBuildTarget {
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
    })
}

/// Resolves a freestanding target while preserving external built-in targets.
pub(crate) fn freestanding_build_target_for(target: &str) -> BareBuildTarget {
    bare_build_target_for(target).unwrap_or_else(|| BareBuildTarget {
        target: target.to_string(),
        cargo_args: vec!["-Z".to_string(), "build-std=core,alloc".to_string()],
        env: HashMap::new(),
    })
}
