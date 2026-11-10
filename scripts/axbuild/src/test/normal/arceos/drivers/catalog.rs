//! Shared host declaration used by axbuild and the driver image build script.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

pub const BATCH_ENV: &str = "AX_DRIVERS_BATCH";
pub const CASES_ENV: &str = "AX_DRIVERS_CASES";
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    schema_version: u32,
    pub run_order: Vec<String>,
    pub batches: BTreeMap<String, Batch>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub arch: String,
    pub build_config: PathBuf,
    pub qemu_config: PathBuf,
    #[serde(default)]
    pub features: Vec<String>,
    pub tests: Vec<Test>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    pub id: String,
    pub name: String,
    pub entry: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub pcm_waveform: bool,
}
pub struct Selection<'a> {
    pub id: &'a str,
    pub batch: &'a Batch,
    pub tests: Vec<&'a Test>,
}
impl Catalog {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        Self::parse(&fs::read_to_string(root.join("suite.toml")).context("read drivers suite")?)
    }
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(source).context("parse drivers suite")?;
        let order: BTreeSet<_> = catalog.run_order.iter().collect();
        ensure!(
            catalog.schema_version == 1
                && !order.is_empty()
                && order.len() == catalog.run_order.len()
                && order.len() == catalog.batches.len()
                && order.iter().all(|id| catalog.batches.contains_key(*id)),
            "drivers run_order must declare each batch once"
        );
        let mut members = BTreeSet::new();
        for (id, batch) in &catalog.batches {
            ensure!(
                identifier(id)
                    && ["aarch64", "riscv64", "x86_64", "loongarch64"]
                        .contains(&batch.arch.as_str())
                    && !batch.tests.is_empty(),
                "invalid drivers batch or architecture"
            );
            for path in [&batch.build_config, &batch.qemu_config] {
                ensure!(
                    !path.as_os_str().is_empty()
                        && path
                            .components()
                            .all(|part| matches!(part, Component::Normal(_))),
                    "driver configuration path escapes suite"
                );
            }
            for test in &batch.tests {
                ensure!(
                    identifier(&test.id)
                        && members.insert(&test.id)
                        && !catalog.batches.contains_key(&test.id)
                        && !test.name.trim().is_empty(),
                    "invalid or ambiguous drivers member"
                );
                ensure!(
                    test.entry.starts_with("crate::")
                        && test.entry.split("::").all(|part| !part.is_empty()
                            && part
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')),
                    "invalid drivers entry"
                );
            }
        }
        Ok(catalog)
    }
    pub fn select(
        &self,
        arch: &str,
        requested: Option<&str>,
    ) -> anyhow::Result<Vec<Selection<'_>>> {
        let requested: Vec<_> = requested
            .map(|text| text.split(',').map(str::trim).collect())
            .unwrap_or_default();
        ensure!(
            requested.iter().all(|id| !id.is_empty()),
            "empty drivers selector"
        );
        for id in &requested {
            ensure!(
                self.batches.iter().any(|(name, batch)| batch.arch == arch
                    && (name == id || batch.tests.iter().any(|test| test.id == *id))),
                "unknown drivers selector `{id}` for {arch}"
            );
        }
        let mut result = Vec::new();
        for id in &self.run_order {
            let batch = &self.batches[id];
            if batch.arch != arch {
                continue;
            }
            let all = requested.is_empty() || requested.contains(&id.as_str());
            let tests: Vec<_> = batch
                .tests
                .iter()
                .filter(|test| all || requested.contains(&test.id.as_str()))
                .collect();
            if !tests.is_empty() {
                result.push(Selection { id, batch, tests });
            }
        }
        ensure!(
            !result.is_empty(),
            "no drivers tests for architecture {arch}"
        );
        Ok(result)
    }
}
impl Selection<'_> {
    pub fn features(&self) -> Vec<String> {
        self.batch
            .features
            .iter()
            .chain(self.batch.tests.iter().flat_map(|test| &test.features))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn members(&self) -> Vec<&str> {
        self.tests.iter().map(|test| test.id.as_str()).collect()
    }
    pub fn selection(&self) -> String {
        if self.tests.len() == self.batch.tests.len() {
            "all".into()
        } else {
            self.members().join(",")
        }
    }
    pub fn completion(&self) -> String {
        format!(
            "DRIVERS_BATCH_OK batch={} selection={}",
            self.id,
            self.selection()
        )
    }
    pub fn build_env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (BATCH_ENV.into(), self.id.into()),
            (CASES_ENV.into(), self.members().join(",")),
        ])
    }
    pub fn verifies_pcm(&self) -> bool {
        self.tests.iter().any(|test| test.pcm_waveform)
    }
}
fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = r#"
schema_version=1
run_order=["combined"]
[batches.combined]
arch="x86_64"
build_config="config/build.toml"
qemu_config="config/qemu.toml"
features=["base"]
[[batches.combined.tests]]
id="first"
name="first capability"
entry="crate::first::run"
features=["extra"]
[[batches.combined.tests]]
id="second"
name="second capability"
entry="crate::second::run"
pcm_waveform=true
"#;
    #[test]
    fn selection_unifies_capabilities_without_selecting_extra_calls_or_fixtures() {
        let catalog = Catalog::parse(SOURCE).unwrap();
        let selected = catalog.select("x86_64", Some("first")).unwrap();
        assert_eq!(selected[0].members(), ["first"]);
        assert_eq!(selected[0].features(), ["base", "extra"]);
        assert!(!selected[0].verifies_pcm());
        let all = catalog
            .select("x86_64", Some("second,first,second"))
            .unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].members(), ["first", "second"]);
        assert!(all[0].verifies_pcm());
        assert_ne!(selected[0].build_env(), all[0].build_env());
        assert!(catalog.select("aarch64", None).is_err());
        assert!(catalog.select("x86_64", Some("missing")).is_err());
        assert!(catalog.select("x86_64", Some("first,")).is_err());
    }
    #[test]
    fn invalid_declarations_fail_before_execution() {
        for text in [
            SOURCE.replace("run_order=[\"combined\"]", "run_order=[]"),
            SOURCE.replace("config/build.toml", "../outside.toml"),
            SOURCE.replace("id=\"second\"", "id=\"first\""),
            SOURCE.replace("crate::second::run", "crate::second::run();"),
        ] {
            assert!(Catalog::parse(&text).is_err());
        }
    }
}
