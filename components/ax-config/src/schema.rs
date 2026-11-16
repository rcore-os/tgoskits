use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CommonDefaults, ConfigError, RuntimeEnv, resolve_runtime};

/// Common owned build inputs, before target selection and runtime fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct CommonConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub features: Vec<String>,
    pub rustflags: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub log: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_cpu_num: Option<usize>,
    pub runtime: RuntimeOptions,
}

/// Explicit runtime overrides; absent inputs remain absent until resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeOptions {
    /// Task stack size in bytes, not the early boot stack.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_stack_size: Option<usize>,
    /// Scheduler tick interval in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduler_tick_ms: Option<u64>,
}

/// Logging threshold selected by the final configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Default for CommonConfig {
    fn default() -> Self {
        Self {
            target: None,
            features: Vec::new(),
            rustflags: Vec::new(),
            env: BTreeMap::new(),
            log: CommonDefaults::DEFAULT.log,
            max_cpu_num: CommonDefaults::DEFAULT.max_cpu_num,
            runtime: CommonDefaults::DEFAULT.runtime,
        }
    }
}

impl CommonConfig {
    /// Validate input format, reserved environment keys and explicit runtime values.
    ///
    /// This does not inspect process environment variables, Cargo metadata, or
    /// target descriptions. The final caller must require a resolved target.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(target) = &self.target
            && (target.is_empty() || target.contains('\0'))
        {
            return Err(ConfigError::invalid(
                "target",
                target,
                "a nonempty target name or target description path without NUL",
            ));
        }
        for feature in &self.features {
            if feature.is_empty() || feature.chars().any(|c| c.is_whitespace() || c == '\0') {
                return Err(ConfigError::invalid(
                    "features",
                    feature,
                    "a nonempty Cargo feature name without whitespace or NUL",
                ));
            }
        }
        for flag in &self.rustflags {
            if flag.contains('\0') {
                return Err(ConfigError::invalid(
                    "rustflags",
                    flag,
                    "an argument without NUL",
                ));
            }
        }
        for (key, value) in &self.env {
            if !valid_env_key(key) {
                return Err(ConfigError::invalid(
                    format!("env.{key}"),
                    key,
                    "a nonempty environment name without '=' or NUL",
                ));
            }
            let replacement = match key.as_str() {
                "SMP" => Some("use max_cpu_num instead of env.SMP"),
                "AX_LOG" => Some("use log instead of env.AX_LOG"),
                "AX_TASK_STACK_SIZE" => Some("use runtime.task_stack_size instead"),
                "AX_SCHEDULER_TICK_MS" => Some("use runtime.scheduler_tick_ms instead"),
                _ => None,
            };
            if let Some(expected) = replacement {
                return Err(ConfigError::invalid(format!("env.{key}"), value, expected));
            }
            if value.contains('\0') {
                return Err(ConfigError::invalid(
                    format!("env.{key}"),
                    value,
                    "an environment value without NUL",
                ));
            }
        }
        resolve_runtime(self.max_cpu_num, self.runtime, &RuntimeEnv::default())?;
        Ok(())
    }
}

pub(crate) fn valid_env_key(key: &str) -> bool {
    !key.is_empty() && !key.contains(['=', '\0'])
}

/// Generate the user-input Schema from the real deserialization contract.
///
/// Control keys share the same generator and definitions as `T`. A collision
/// with an ordinary field is an implementation error, never an override.
#[cfg(all(feature = "schema", feature = "toml"))]
pub fn input_schema<T: schemars::JsonSchema>() -> Result<schemars::Schema, ConfigError> {
    use schemars::{
        JsonSchema,
        generate::{Contract, SchemaSettings},
    };

    use crate::OverlayControls;

    let mut generator = SchemaSettings::default()
        .with(|settings| settings.contract = Contract::Deserialize)
        .into_generator();
    let controls = OverlayControls::json_schema(&mut generator);
    let mut root = generator.into_root_schema_for::<T>();
    let control_properties = controls
        .get("properties")
        .and_then(|value| value.as_object())
        .ok_or_else(|| ConfigError::Schema {
            reason: "overlay controls must have object properties".into(),
        })?;
    let properties = root
        .get_mut("properties")
        .and_then(|value| value.as_object_mut())
        .ok_or_else(|| ConfigError::Schema {
            reason: "configuration root must have object properties".into(),
        })?;
    for key in control_properties.keys() {
        if properties.contains_key(key) {
            return Err(ConfigError::Schema {
                reason: format!("control key {key:?} conflicts with a configuration field"),
            });
        }
    }
    properties.extend(control_properties.clone());
    root.insert("additionalProperties".into(), false.into());
    Ok(root)
}
