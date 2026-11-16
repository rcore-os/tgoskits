//! Stable identities for serialized build inputs, independent of map iteration order.
use serde::Serialize;
use sha2::{Digest, Sha256};

pub(crate) fn signature(value: &impl Serialize) -> anyhow::Result<String> {
    fn ordered(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut rows: Vec<_> = map.into_iter().collect();
                rows.sort_by(|a, b| a.0.cmp(&b.0));
                serde_json::Value::Object(rows.into_iter().map(|(k, v)| (k, ordered(v))).collect())
            }
            serde_json::Value::Array(rows) => {
                serde_json::Value::Array(rows.into_iter().map(ordered).collect())
            }
            value => value,
        }
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&ordered(serde_json::to_value(value)?))?)
    ))
}
