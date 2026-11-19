//! Optional timing hints. They schedule checks but never select their membership.
use std::{collections::BTreeMap, path::Path};

use anyhow::{Context, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::check::ClippyCheck;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TimingWeights {
    schema_version: u32,
    rustc: String,
    source: String,
    checks: BTreeMap<String, f64>,
}

impl TimingWeights {
    pub(super) fn load(path: &Path, rustc: &str) -> anyhow::Result<Option<Self>> {
        let weights: Self = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("read Clippy timing weights {}", path.display()))?,
        )
        .context("parse Clippy timing weights")?;
        ensure!(
            weights.schema_version == 1,
            "unsupported Clippy timing schema"
        );
        ensure!(!weights.checks.is_empty(), "empty Clippy timing weights");
        ensure!(
            weights.checks.iter().all(|(id, cost)| id.len() == 64
                && id.bytes().all(|byte| byte.is_ascii_hexdigit())
                && cost.is_finite()
                && *cost > 0.0),
            "invalid Clippy timing identity or duration"
        );
        ensure!(
            weights.checks.values().sum::<f64>().is_finite(),
            "Clippy timing sum overflow"
        );
        if weights.rustc != rustc {
            println!("CLIPPY_TIMING_WEIGHTS: toolchain changed; using count-balanced scheduling");
            return Ok(None);
        }
        println!("CLIPPY_TIMING_WEIGHTS: source={}", weights.source);
        Ok(Some(weights))
    }

    pub(super) fn costs(&self, checks: &[ClippyCheck]) -> anyhow::Result<Vec<f64>> {
        let fallback = self.checks.values().sum::<f64>() / self.checks.len() as f64;
        let mut matched = 0;
        let costs = checks
            .iter()
            .map(|check| {
                let id = Self::identity(check)?;
                Ok(if let Some(cost) = self.checks.get(&id) {
                    matched += 1;
                    *cost
                } else {
                    fallback
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        ensure!(
            costs.iter().sum::<f64>().is_finite(),
            "Clippy estimated cost overflow"
        );
        println!(
            "CLIPPY_TIMING_WEIGHTS: matched={matched} unknown={} (all checks retained)",
            checks.len() - matched
        );
        Ok(costs)
    }

    pub(super) fn identity(check: &ClippyCheck) -> anyhow::Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(check)?)))
    }
}
