use std::path::{Path, PathBuf};

use figment::{
    Figment, Metadata, Profile, Provider,
    providers::Serialized,
    value::{Dict, Map},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{ConfigError, schema::valid_env_key};

const CONTROL_KEYS: [&str; 4] = ["preset", "features_add", "features_remove", "unset"];

/// Input-only selection and editing controls, not final configuration fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OverlayControls {
    pub preset: Option<String>,
    pub features_add: Vec<String>,
    pub features_remove: Vec<String>,
    pub unset: Vec<String>,
}

/// One parsed user input with ordinary fields still sparse and untyped.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub controls: OverlayControls,
    fields: toml::Table,
    source: PathBuf,
}

impl Overlay {
    /// Whether the original input explicitly supplied an ordinary root field.
    pub fn contains_field(&self, key: &str) -> bool {
        self.fields.contains_key(key)
    }

    /// Original source identity, used for diagnostics and field-specific paths.
    pub fn source(&self) -> &Path {
        &self.source
    }
}

/// Parse already-read TOML once and separate the four input-only control keys.
///
/// This never opens `source`, applies defaults, or validates OS-specific fields.
pub fn parse_overlay(source: &Path, text: &str) -> Result<Overlay, ConfigError> {
    let mut fields: toml::Table =
        toml::from_str(text).map_err(|source_error| ConfigError::Syntax {
            origin: source.to_owned(),
            source: source_error,
        })?;
    let mut controls = toml::Table::new();
    for key in CONTROL_KEYS {
        if let Some(value) = fields.remove(key) {
            controls.insert(key.into(), value);
        }
    }
    let controls = Figment::from(FileOverlay {
        path: source,
        fields: &controls,
    })
    .extract()
    .map_err(|error| extraction_error(source, error))?;
    Ok(Overlay {
        controls,
        fields,
        source: source.to_owned(),
    })
}

/// Apply finite clears, ordinary Figment merge, then stable feature edits.
///
/// `os_clearable_fields` may enable `app-c` or `fit_its` for the owning OS.
/// Ordinary arrays replace; tables merge recursively; empty tables do not clear.
/// Final typed extraction is strict and happens once. The caller subsequently
/// applies environment FEATURES, explicit CLI inputs, and final OS validation.
pub fn apply_overlay<T>(
    base: &T,
    overlay: &Overlay,
    os_clearable_fields: &[&str],
) -> Result<T, ConfigError>
where
    T: Serialize + DeserializeOwned,
{
    validate_controls(overlay)?;
    let base_value = toml::Value::try_from(base).map_err(|source| ConfigError::Type {
        origin: None,
        field: "<base>".into(),
        source: Box::new(source),
    })?;
    let toml::Value::Table(mut base_table) = base_value else {
        return Err(invalid(overlay, "<base>", "", "a configuration table"));
    };
    for path in &overlay.controls.unset {
        clear_field(&mut base_table, path, os_clearable_fields, overlay)?;
    }
    let mut merged = Figment::from(Serialized::defaults(&base_table)).merge(FileOverlay {
        path: overlay.source(),
        fields: &overlay.fields,
    });
    if !overlay.controls.features_add.is_empty() || !overlay.controls.features_remove.is_empty() {
        let mut features: Vec<String> = merged
            .extract_inner("features")
            .map_err(|error| extraction_error(overlay.source(), error))?;
        for feature in &overlay.controls.features_add {
            if !features.contains(feature) {
                features.push(feature.clone());
            }
        }
        features.retain(|feature| !overlay.controls.features_remove.contains(feature));
        let mut edited = toml::Table::new();
        edited.insert(
            "features".into(),
            toml::Value::Array(features.into_iter().map(toml::Value::String).collect()),
        );
        merged = merged.merge(FileOverlay {
            path: overlay.source(),
            fields: &edited,
        });
    }
    merged
        .extract()
        .map_err(|error| extraction_error(overlay.source(), error))
}

/// Supplies already-read input; Figment must never perform a second file read.
struct FileOverlay<'a> {
    path: &'a Path,
    fields: &'a toml::Table,
}

impl Provider for FileOverlay<'_> {
    fn metadata(&self) -> Metadata {
        Metadata::from("TOML override", self.path)
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        Serialized::defaults(self.fields).data()
    }
}

fn validate_controls(overlay: &Overlay) -> Result<(), ConfigError> {
    for (field, features) in [
        ("features_add", &overlay.controls.features_add),
        ("features_remove", &overlay.controls.features_remove),
    ] {
        for feature in features {
            if feature.is_empty() || feature.chars().any(|c| c.is_whitespace() || c == '\0') {
                return Err(invalid(
                    overlay,
                    field,
                    feature,
                    "nonempty feature names without whitespace or NUL",
                ));
            }
        }
    }
    for feature in &overlay.controls.features_add {
        if overlay.controls.features_remove.contains(feature) {
            return Err(ConfigError::SelectorConflict {
                origin: Some(overlay.source.clone()),
                field: "features_add/features_remove".into(),
                expected: format!("add {feature}"),
                actual: format!("remove {feature}"),
            });
        }
    }
    Ok(())
}

fn clear_field(
    base: &mut toml::Table,
    path: &str,
    os_clearable_fields: &[&str],
    overlay: &Overlay,
) -> Result<(), ConfigError> {
    match path {
        "max_cpu_num" => {
            base.remove(path);
        }
        "env" => {
            base.remove("env");
        }
        "runtime.task_stack_size" | "runtime.scheduler_tick_ms" => {
            if let Some(toml::Value::Table(runtime)) = base.get_mut("runtime") {
                let key = if path == "runtime.task_stack_size" {
                    "task_stack_size"
                } else {
                    "scheduler_tick_ms"
                };
                runtime.remove(key);
            }
        }
        "app-c" | "fit_its" if os_clearable_fields.contains(&path) => {
            base.remove(path);
        }
        _ => {
            if let Some(key) = path.strip_prefix("env.")
                && valid_env_key(key)
            {
                if let Some(toml::Value::Table(env)) = base.get_mut("env") {
                    env.remove(key);
                }
            } else {
                return Err(invalid(
                    overlay,
                    "unset",
                    path,
                    "max_cpu_num, runtime.task_stack_size, runtime.scheduler_tick_ms, env, \
                     env.KEY, or an allowed OS optional field",
                ));
            }
        }
    }
    Ok(())
}

fn extraction_error(origin: &Path, source: figment::Error) -> ConfigError {
    let mut path = source.path.clone();
    if let figment::error::Kind::UnknownField(key, _) = &source.kind {
        if path.last() != Some(key) {
            path.push(key.clone());
        }
        let hint = match key.as_str() {
            "std" => Some("select the appropriate Cargo features instead"),
            "plat_dyn" => Some("dynamic platform selection is automatic; remove plat_dyn"),
            _ => None,
        };
        return ConfigError::UnknownField {
            origin: Some(origin.to_owned()),
            field: path.join("."),
            hint,
            source: Some(Box::new(source)),
        };
    }
    let field = if path.is_empty() {
        "<root>".into()
    } else {
        path.join(".")
    };
    ConfigError::Type {
        origin: Some(origin.to_owned()),
        field,
        source: Box::new(source),
    }
}

fn invalid(overlay: &Overlay, field: &str, value: &str, expected: &'static str) -> ConfigError {
    ConfigError::InvalidValue {
        origin: Some(overlay.source.clone()),
        field: field.into(),
        value: value.into(),
        expected,
    }
}
