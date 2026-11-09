use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    schema_version: u32,
    pub(crate) run_order: Vec<String>,
    pub(crate) batches: BTreeMap<String, Batch>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Batch {
    pub(crate) arch: String,
    pub(crate) image: Image,
    pub(crate) mode: Mode,
    pub(crate) build_config: PathBuf,
    pub(crate) qemu_config: PathBuf,
    #[serde(default)]
    pub(crate) pin_to_boot_cpu: bool,
    #[serde(default)]
    pub(crate) features: Vec<String>,
    pub(crate) tests: Vec<Test>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Image {
    Std,
    Freestanding,
}

impl Image {
    pub(crate) fn package(self) -> &'static str {
        match self {
            Self::Std => "arceos-cpu-normal-std",
            Self::Freestanding => "arceos-cpu-normal-freestanding",
        }
    }
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
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) entry: String,
    #[serde(default)]
    pub(crate) aliases: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct Selection<'a> {
    pub(crate) id: &'a str,
    pub(crate) batch: &'a Batch,
    pub(crate) tests: Vec<&'a Test>,
    pub(crate) selected_test: Option<&'a str>,
}

impl Selection<'_> {
    pub(crate) fn features(&self) -> Vec<String> {
        let mut features = self.batch.features.clone();
        features.extend(
            self.tests
                .iter()
                .map(|test| format!("cpu-tests/{}", test.id)),
        );
        features.sort();
        features.dedup();
        features
    }
    pub(crate) fn label(&self) -> String {
        self.selected_test
            .map_or_else(|| self.id.to_owned(), |test| format!("{}/{test}", self.id))
    }
    pub(crate) fn build_env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("AX_CPU_TEST_BATCH".to_owned(), self.id.to_owned()),
            (
                "AX_CPU_TEST_CASE".to_owned(),
                self.selected_test.unwrap_or("").to_owned(),
            ),
        ])
    }
    pub(crate) fn completion(&self) -> String {
        format!(
            "CPU_BATCH_OK batch={} selection={}",
            self.id,
            self.selected_test.unwrap_or("all")
        )
    }
}

pub(crate) fn root(workspace: &Path) -> PathBuf {
    workspace.join("test-suit/arceos/cpu/normal")
}

impl Catalog {
    pub(crate) fn load(root: &Path) -> anyhow::Result<Self> {
        let path = root.join("suite.toml");
        Self::parse(&fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?)
    }
    pub(crate) fn parse(source: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(source).context("parse CPU suite declaration")?;
        ensure!(catalog.schema_version == 1, "unsupported CPU suite schema");
        ensure!(
            !catalog.run_order.is_empty(),
            "CPU run_order must not be empty"
        );
        let order: BTreeSet<_> = catalog.run_order.iter().collect();
        ensure!(
            order.len() == catalog.run_order.len(),
            "duplicate CPU batch in run_order"
        );
        ensure!(
            order.len() == catalog.batches.len()
                && order.iter().all(|id| catalog.batches.contains_key(*id)),
            "CPU run_order must contain every declared batch exactly once"
        );
        let mut aliases = BTreeSet::new();
        for (id, batch) in &catalog.batches {
            ensure!(identifier(id), "invalid CPU batch id `{id}`");
            ensure!(
                ["aarch64", "riscv64", "x86_64", "loongarch64"].contains(&batch.arch.as_str()),
                "unsupported CPU architecture `{}`",
                batch.arch
            );
            ensure!(!batch.tests.is_empty(), "CPU batch `{id}` has no tests");
            ensure!(
                batch.mode != Mode::ExpectedFault || batch.tests.len() == 1,
                "expected-fault CPU batch must have one terminating test"
            );
            for path in [&batch.build_config, &batch.qemu_config] {
                ensure!(
                    !path.as_os_str().is_empty()
                        && path
                            .components()
                            .all(|part| matches!(part, Component::Normal(_))),
                    "CPU config path must be relative and stay inside the suite"
                );
            }
            let mut members = BTreeSet::new();
            for test in &batch.tests {
                ensure!(
                    identifier(&test.id) && members.insert(&test.id),
                    "invalid or duplicate CPU test id in `{id}`"
                );
                ensure!(!test.name.trim().is_empty(), "CPU test must have a name");
                ensure!(
                    test.entry.starts_with("cpu_tests::")
                        && test.entry.split("::").all(|part| !part.is_empty()
                            && part
                                .chars()
                                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')),
                    "invalid CPU entry `{}`",
                    test.entry
                );
                for alias in &test.aliases {
                    ensure!(
                        identifier(alias)
                            && !catalog.batches.contains_key(alias)
                            && aliases.insert((&batch.arch, alias)),
                        "ambiguous CPU alias `{alias}` for {}",
                        batch.arch
                    );
                }
            }
        }
        Ok(catalog)
    }
    pub(crate) fn select(
        &self,
        arch: &str,
        selected: Option<&str>,
    ) -> anyhow::Result<Vec<Selection<'_>>> {
        let mut result = Vec::new();
        for id in &self.run_order {
            let batch = &self.batches[id];
            if batch.arch != arch {
                continue;
            }
            if selected.is_none() || selected == Some(id.as_str()) {
                result.push(Selection {
                    id,
                    batch,
                    tests: batch.tests.iter().collect(),
                    selected_test: None,
                });
            } else if let Some(test) = batch.tests.iter().find(|test| {
                let selector = selected.expect("selected case");
                test.aliases.iter().any(|alias| alias == selector)
                    || selector == format!("{id}/{}", test.id)
            }) {
                result.push(Selection {
                    id,
                    batch,
                    tests: vec![test],
                    selected_test: Some(&test.id),
                });
            }
        }
        if result.is_empty() {
            bail!("no CPU tests match architecture `{arch}` and selection {selected:?}");
        }
        Ok(result)
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: &str = r#"
schema_version = 1
run_order = ["later", "first"]
[batches.first]
arch = "riscv64"
image = "std"
mode = "normal"
build_config = "fixture/build.toml"
qemu_config = "fixture/qemu.toml"
[[batches.first.tests]]
id = "copy"
name = "copy capability"
entry = "cpu_tests::copy::run"
aliases = ["old-copy"]
[batches.later]
arch = "riscv64"
image = "std"
mode = "normal"
build_config = "fixture/build.toml"
qemu_config = "fixture/qemu.toml"
[[batches.later.tests]]
id = "map"
name = "mapping capability"
entry = "cpu_tests::map::run"
"#;
    #[test]
    fn selection_preserves_declared_order_and_separates_single_member_identity() {
        let catalog = Catalog::parse(SOURCE).unwrap();
        let all = catalog.select("riscv64", None).unwrap();
        assert_eq!(
            all.iter().map(|item| item.id).collect::<Vec<_>>(),
            ["later", "first"]
        );
        let single = catalog.select("riscv64", Some("old-copy")).unwrap();
        assert_eq!(single[0].tests[0].entry, "cpu_tests::copy::run");
        assert_ne!(single[0].build_env(), all[1].build_env());
        assert_ne!(single[0].completion(), all[1].completion());
        assert!(catalog.select("aarch64", Some("old-copy")).is_err());
    }
    #[test]
    fn malformed_plans_are_rejected_before_any_execution() {
        for source in [
            SOURCE.replace("[\"later\", \"first\"]", "[\"first\", \"first\"]"),
            SOURCE.replace("[\"later\", \"first\"]", "[\"first\"]"),
            SOURCE.replace("fixture/build.toml", "../escape.toml"),
            SOURCE.replace("cpu_tests::map::run", "cpu_tests::map::run();"),
            SOURCE.replace("id = \"map\"", "id = \"\""),
        ] {
            assert!(Catalog::parse(&source).is_err());
        }
    }
}
