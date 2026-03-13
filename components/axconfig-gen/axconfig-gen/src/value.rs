use std::fmt;

use toml_edit::Value;

use crate::{ConfigErr, ConfigResult, ConfigType};

/// A structure representing a config value.
#[derive(Clone)]
pub struct ConfigValue {
    value: Value,
    ty: Option<ConfigType>,
}

impl ConfigValue {
    /// Parses a TOML-formatted string into a [`ConfigValue`].
    pub fn new(s: &str) -> ConfigResult<Self> {
        let value = s.parse::<Value>()?;
        Self::from_raw_value(&value)
    }

    /// Parses a TOML-formatted string into a [`ConfigValue`] with a specified type.
    pub fn new_with_type(s: &str, ty: &str) -> ConfigResult<Self> {
        let value = s.parse::<Value>()?;
        let ty = ConfigType::new(ty)?;
        Self::from_raw_value_type(&value, ty)
    }

    pub(crate) fn from_raw_value(value: &Value) -> ConfigResult<Self> {
        if !value_is_valid(value) {
            return Err(ConfigErr::InvalidValue);
        }
        Ok(Self {
            value: value.clone(),
            ty: None,
        })
    }

    pub(crate) fn from_raw_value_type(value: &Value, ty: ConfigType) -> ConfigResult<Self> {
        if !value_is_valid(value) {
            return Err(ConfigErr::InvalidValue);
        }
        if value_type_matches(value, &ty) {
            Ok(Self {
                value: value.clone(),
                ty: Some(ty),
            })
        } else {
            Err(ConfigErr::ValueTypeMismatch)
        }
    }

    /// Returns the type of the config value if it is specified on construction.
    pub fn ty(&self) -> Option<&ConfigType> {
        self.ty.as_ref()
    }

    /// Updates the config value with a new value.
    pub fn update(&mut self, new_value: Self) -> ConfigResult<()> {
        match (&self.ty, &new_value.ty) {
            (Some(ty), Some(new_ty)) => {
                if ty != new_ty {
                    return Err(ConfigErr::ValueTypeMismatch);
                }
            }
            (Some(ty), None) => {
                if !value_type_matches(&new_value.value, ty) {
                    return Err(ConfigErr::ValueTypeMismatch);
                }
            }
            (None, Some(new_ty)) => {
                if !value_type_matches(&self.value, new_ty) {
                    return Err(ConfigErr::ValueTypeMismatch);
                }
                self.ty = new_value.ty;
            }
            _ => {}
        }
        self.value = new_value.value;
        Ok(())
    }

    /// Returns the inferred type of the config value.
    pub fn inferred_type(&self) -> ConfigResult<ConfigType> {
        inferred_type(&self.value)
    }

    /// Returns whether the type of the config value matches the specified type.
    pub fn type_matches(&self, ty: &ConfigType) -> bool {
        value_type_matches(&self.value, ty)
    }

    /// Returns the TOML-formatted string of the config value.
    pub fn to_toml_value(&self) -> String {
        to_toml(&self.value)
    }

    /// Returns the Rust code of the config value.
    ///
    /// The `indent` parameter specifies the number of spaces to indent the code.
    pub fn to_rust_value(&self, ty: &ConfigType, indent: usize) -> ConfigResult<String> {
        to_rust(&self.value, ty, indent)
    }
}

impl fmt::Debug for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigValue")
            .field("value", &self.to_toml_value())
            .field("type", &self.ty)
            .finish()
    }
}

fn is_num(s: &str) -> bool {
    let s = s.to_lowercase().replace('_', "");
    if let Some(s) = s.strip_prefix("0x") {
        usize::from_str_radix(s, 16).is_ok()
    } else if let Some(s) = s.strip_prefix("0b") {
        usize::from_str_radix(s, 2).is_ok()
    } else if let Some(s) = s.strip_prefix("0o") {
        usize::from_str_radix(s, 8).is_ok()
    } else {
        s.parse::<usize>().is_ok()
    }
}

fn value_is_valid(value: &Value) -> bool {
    match value {
        Value::Boolean(_) | Value::Integer(_) | Value::String(_) => true,
        Value::Array(arr) => {
            for e in arr {
                if !value_is_valid(e) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

fn value_type_matches(value: &Value, ty: &ConfigType) -> bool {
    match (value, ty) {
        (Value::Boolean(_), ConfigType::Bool) => true,
        (Value::Integer(_), ConfigType::Int | ConfigType::Uint) => true,
        (Value::String(s), _) => {
            let s = s.value();
            if is_num(s) {
                matches!(ty, ConfigType::Int | ConfigType::Uint | ConfigType::String)
            } else {
                matches!(ty, ConfigType::String)
            }
        }
        (Value::Array(arr), ConfigType::Tuple(ty)) => {
            if arr.len() != ty.len() {
                return false;
            }
            for (e, t) in arr.iter().zip(ty.iter()) {
                if !value_type_matches(e, t) {
                    return false;
                }
            }
            true
        }
        (Value::Array(arr), ConfigType::Array(ty)) => {
            for e in arr {
                if !value_type_matches(e, ty) {
                    return false;
                }
            }
            true
        }
        _ => false,
    }
}

fn inferred_type(value: &Value) -> ConfigResult<ConfigType> {
    match value {
        Value::Boolean(_) => Ok(ConfigType::Bool),
        Value::Integer(i) => {
            let val = *i.value();
            if val < 0 {
                Ok(ConfigType::Int)
            } else {
                Ok(ConfigType::Uint)
            }
        }
        Value::String(s) => {
            let s = s.value();
            if is_num(s) {
                Ok(ConfigType::Uint)
            } else {
                Ok(ConfigType::String)
            }
        }
        Value::Array(arr) => {
            let types = arr
                .iter()
                .map(inferred_type)
                .collect::<ConfigResult<Vec<_>>>()?;
            if types.is_empty() {
                return Ok(ConfigType::Unknown);
            }

            let mut all_same = true;
            for t in types.iter() {
                if matches!(t, ConfigType::Unknown) {
                    return Ok(ConfigType::Unknown);
                }
                if t != &types[0] {
                    all_same = false;
                    break;
                }
            }

            if all_same {
                Ok(ConfigType::Array(Box::new(types[0].clone())))
            } else {
                Ok(ConfigType::Tuple(types))
            }
        }
        _ => Err(ConfigErr::InvalidValue),
    }
}

pub fn to_toml(value: &Value) -> String {
    match &value {
        Value::Boolean(b) => b.display_repr().to_string(),
        Value::Integer(i) => i.display_repr().to_string(),
        Value::String(s) => s.display_repr().to_string(),
        Value::Array(arr) => {
            let elements = arr.iter().map(to_toml).collect::<Vec<_>>();
            if arr.iter().any(|e| e.is_array()) {
                format!("[\n    {}\n]", elements.join(",\n").replace("\n", "\n    "))
            } else {
                format!("[{}]", elements.join(", "))
            }
        }
        _ => "".to_string(),
    }
}

pub fn to_rust(value: &Value, ty: &ConfigType, indent: usize) -> ConfigResult<String> {
    match (value, ty) {
        (Value::Boolean(b), ConfigType::Bool) => Ok(b.display_repr().to_string()),
        (Value::Integer(i), ConfigType::Int | ConfigType::Uint) => Ok(i.display_repr().to_string()),
        (Value::String(s), _) => {
            if matches!(ty, ConfigType::Int | ConfigType::Uint) {
                Ok(s.value().to_string())
            } else if matches!(ty, ConfigType::String) {
                Ok(s.display_repr().to_string())
            } else {
                Err(ConfigErr::ValueTypeMismatch)
            }
        }
        (Value::Array(arr), ConfigType::Tuple(ty)) => {
            if arr.len() != ty.len() {
                return Err(ConfigErr::ValueTypeMismatch);
            }
            let elements = arr
                .iter()
                .zip(ty)
                .map(|(v, t)| to_rust(v, t, indent))
                .collect::<ConfigResult<Vec<_>>>()?;
            Ok(format!("({})", elements.join(", ")))
        }
        (Value::Array(arr), ConfigType::Array(ty)) => {
            let elements = arr
                .iter()
                .map(|v| to_rust(v, ty, indent + 4))
                .collect::<ConfigResult<Vec<_>>>()?;
            let code = if arr.iter().any(|e| e.is_array()) {
                let spaces = format!("\n{:indent$}", "", indent = indent + 4);
                let spaces_end = format!(",\n{:indent$}", "", indent = indent);
                format!(
                    "&[{}{}{}]",
                    spaces,
                    elements.join(&format!(",{}", spaces)),
                    spaces_end
                )
            } else {
                format!("&[{}]", elements.join(", "))
            };
            Ok(code)
        }
        _ => Err(ConfigErr::ValueTypeMismatch),
    }
}
