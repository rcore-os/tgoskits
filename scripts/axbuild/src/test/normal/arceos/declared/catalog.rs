//! Ordered execution declarations; repository-specific identities live in the input.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    schema_version: u32,
    pub run_order: Vec<String>,
    pub execution: Execution,
    pub batches: BTreeMap<String, Batch>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    pub batch_env: String,
    pub members_env: String,
    pub all_members_value: Option<String>,
    pub completion: String,
    #[serde(default)]
    pub feature_scope: FeatureScope,
    #[serde(default)]
    pub selection_mode: SelectionMode,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FeatureScope {
    #[default]
    Selected,
    Batch,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SelectionMode {
    Single,
    #[default]
    Multiple,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Batch {
    pub arch: String,
    pub package: String,
    pub image: Image,
    pub mode: Mode,
    pub build_config: PathBuf,
    pub qemu_config: PathBuf,
    #[serde(default)]
    pub pin_to_boot_cpu: bool,
    #[serde(default)]
    pub features: Vec<String>,
    pub tests: Vec<Test>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Image {
    Std,
    Freestanding,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mode {
    Normal,
    ExpectedFault,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Test {
    pub id: String,
    pub name: String,
    pub entry: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub pcm_waveform: bool,
}

#[derive(Debug)]
pub(crate) struct Selection<'a> {
    pub id: &'a str,
    pub batch: &'a Batch,
    pub tests: Vec<&'a Test>,
    execution: &'a Execution,
    all: bool,
}

impl Selection<'_> {
    pub fn features(&self) -> Vec<String> {
        let tests: Vec<_> = if self.execution.feature_scope == FeatureScope::Batch {
            self.batch.tests.iter().collect()
        } else {
            self.tests.clone()
        };
        self.batch
            .features
            .iter()
            .chain(tests.iter().flat_map(|test| &test.features))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn members(&self) -> Vec<&str> {
        self.tests.iter().map(|test| test.id.as_str()).collect()
    }
    pub fn selection(&self) -> String {
        if self.all {
            "all".into()
        } else {
            self.members().join(",")
        }
    }
    pub fn label(&self) -> String {
        if self.all {
            self.id.to_owned()
        } else {
            format!("{}/{}", self.id, self.selection())
        }
    }
    pub fn build_env(&self) -> BTreeMap<String, String> {
        let members = if self.all {
            self.execution
                .all_members_value
                .clone()
                .unwrap_or_else(|| self.members().join(","))
        } else {
            self.members().join(",")
        };
        BTreeMap::from([
            (self.execution.batch_env.clone(), self.id.to_owned()),
            (self.execution.members_env.clone(), members),
        ])
    }
    pub fn completion(&self) -> String {
        self.execution
            .completion
            .replace("{batch}", self.id)
            .replace("{selection}", &self.selection())
    }
    pub fn verifies_pcm(&self) -> bool {
        self.tests.iter().any(|test| test.pcm_waveform)
    }
}

pub(crate) fn relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

impl Catalog {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)
    }
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(source).context("parse execution suite")?;
        let order: BTreeSet<_> = catalog.run_order.iter().collect();
        ensure!(
            catalog.schema_version == 1
                && !order.is_empty()
                && order.len() == catalog.run_order.len()
                && order.len() == catalog.batches.len()
                && order.iter().all(|id| catalog.batches.contains_key(*id)),
            "run_order must contain every declared batch exactly once"
        );
        let e = &catalog.execution;
        let env_name = |name: &str| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        ensure!(
            env_name(&e.batch_env) && env_name(&e.members_env) && e.batch_env != e.members_env,
            "batch and member environment names must be distinct identifiers"
        );
        let marker = e
            .completion
            .replace("{batch}", "")
            .replace("{selection}", "");
        ensure!(
            !e.completion.trim().is_empty()
                && !e.completion.chars().any(char::is_control)
                && e.completion.contains("{batch}")
                && e.completion.contains("{selection}")
                && !marker.contains(['{', '}']),
            "completion must bind batch and selection only"
        );
        let mut aliases = BTreeSet::new();
        for (id, batch) in &catalog.batches {
            ensure!(
                identifier(id)
                    && identifier(&batch.package)
                    && crate::context::supported_arches().contains(&batch.arch.as_str())
                    && !batch.tests.is_empty(),
                "invalid batch identity, architecture or members"
            );
            ensure!(
                batch.mode != Mode::ExpectedFault || batch.tests.len() == 1,
                "expected-fault batch must have one terminating member"
            );
            ensure!(
                relative_path(&batch.build_config) && relative_path(&batch.qemu_config),
                "configuration paths must stay inside the suite"
            );
            let mut members = BTreeSet::new();
            for test in &batch.tests {
                ensure!(
                    identifier(&test.id)
                        && members.insert(&test.id)
                        && !test.name.trim().is_empty(),
                    "invalid or duplicate member in {id}"
                );
                syn::parse_str::<syn::Path>(&test.entry).context("invalid target entry path")?;
                for alias in &test.aliases {
                    ensure!(
                        identifier(alias)
                            && !catalog.batches.contains_key(alias)
                            && aliases.insert((&batch.arch, alias)),
                        "ambiguous member alias {alias}"
                    );
                }
            }
        }
        Ok(catalog)
    }
    pub fn select(
        &self,
        arch: &str,
        requested: Option<&str>,
    ) -> anyhow::Result<Vec<Selection<'_>>> {
        let selectors: Vec<_> = requested
            .map(|text| text.split(',').map(str::trim).collect())
            .unwrap_or_default();
        ensure!(
            selectors.iter().all(|s| !s.is_empty()),
            "empty member selector"
        );
        ensure!(
            self.execution.selection_mode != SelectionMode::Single || selectors.len() <= 1,
            "this suite accepts one selection at a time"
        );
        let available: Vec<_> = self
            .run_order
            .iter()
            .filter_map(|id| {
                let batch = &self.batches[id];
                (batch.arch == arch).then_some((id.as_str(), batch))
            })
            .collect();
        // Aliases are explicit. Bare member IDs are accepted only when unambiguous.
        let mut selected_members = BTreeSet::new();
        let mut selected_batches = BTreeSet::new();
        for selector in &selectors {
            if available.iter().any(|(id, _)| id == selector) {
                selected_batches.insert(*selector);
                continue;
            }
            let mut matches: Vec<_> = available
                .iter()
                .flat_map(|(id, batch)| {
                    batch
                        .tests
                        .iter()
                        .filter(move |test| {
                            test.aliases.iter().any(|a| a == selector)
                                || *selector == format!("{id}/{}", test.id)
                        })
                        .map(move |test| (*id, test.id.as_str()))
                })
                .collect();
            if matches.is_empty() {
                matches = available
                    .iter()
                    .flat_map(|(id, batch)| {
                        batch
                            .tests
                            .iter()
                            .filter(|test| test.id == *selector)
                            .map(move |test| (*id, test.id.as_str()))
                    })
                    .collect();
            }
            ensure!(
                matches.len() == 1,
                "unknown or ambiguous selection {selector} for {arch}"
            );
            selected_members.insert(matches[0]);
        }
        let mut result = Vec::new();
        for (id, batch) in available {
            let whole = selectors.is_empty() || selected_batches.contains(id);
            let tests: Vec<_> = batch
                .tests
                .iter()
                .filter(|test| whole || selected_members.contains(&(id, test.id.as_str())))
                .collect();
            if !tests.is_empty() {
                let all = whole
                    || (self.execution.selection_mode == SelectionMode::Multiple
                        && tests.len() == batch.tests.len());
                result.push(Selection {
                    id,
                    batch,
                    tests,
                    execution: &self.execution,
                    all,
                });
            }
        }
        ensure!(
            !result.is_empty(),
            "no declared tests for {arch} and {requested:?}"
        );
        Ok(result)
    }
}
fn identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = r#"
schema_version=1
run_order=["transfer"]
[execution]
batch_env="DEMO_BATCH"
members_env="DEMO_MEMBERS"
completion="DEMO_OK batch={batch} selection={selection}"
[batches.transfer]
arch="riscv64"
package="external-image"
image="std"
mode="normal"
build_config="build.toml"
qemu_config="run.toml"
features=["base"]
[[batches.transfer.tests]]
id="receive"
name="receive a message"
entry="custom::receive"
features=["transport/receive"]
aliases=["old-receive"]
[[batches.transfer.tests]]
id="close"
name="close transport"
entry="custom::close"
features=["transport/close"]
pcm_waveform=true
"#;
    #[test]
    fn selection_preserves_order_capabilities_and_completion_identity() {
        let catalog = Catalog::parse(SOURCE).unwrap();
        let single = catalog.select("riscv64", Some("old-receive")).unwrap();
        assert_eq!(single[0].members(), ["receive"]);
        assert_eq!(single[0].features(), ["base", "transport/receive"]);
        assert!(!single[0].verifies_pcm());
        let all = catalog
            .select("riscv64", Some("close,receive,close"))
            .unwrap();
        assert_eq!(all[0].members(), ["receive", "close"]);
        assert!(all[0].verifies_pcm());
        assert_ne!(single[0].completion(), all[0].completion());
        assert_ne!(single[0].build_env(), all[0].build_env());
        assert!(catalog.select("riscv64", Some("missing")).is_err());
        assert!(catalog.select("riscv64", Some("receive,")).is_err());
        assert!(catalog.select("aarch64", None).is_err());
    }
    #[test]
    fn malformed_declarations_fail_before_execution() {
        for text in [
            SOURCE.replace("[\"transfer\"]", "[]"),
            SOURCE.replace("build.toml", "../escape.toml"),
            SOURCE.replace("id=\"close\"", "id=\"receive\""),
            SOURCE.replace("custom::close", "custom::close();"),
            SOURCE.replace("DEMO_MEMBERS", "DEMO_BATCH"),
            SOURCE.replace("{batch}", "{unknown}"),
        ] {
            assert!(
                Catalog::parse(&text).is_err(),
                "accepted malformed declaration"
            );
        }
    }
}
