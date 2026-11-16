//! Host-side board declarations shared by axbuild and the two binding generators.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::Deserialize;

pub const SUITE_ENV: &str = "AX_BOARD_TEST_SUITE";
pub const BATCH_ENV: &str = "AX_BOARD_TEST_BATCH";
pub const CASES_ENV: &str = "AX_BOARD_TEST_CASES";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    schema_version: u32,
    pub board: String,
    pub run_order: Vec<String>,
    pub batches: BTreeMap<String, Batch>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub arch: String,
    pub image: Image,
    pub executor: Executor,
    pub package: String,
    pub build_config: PathBuf,
    pub board_config: PathBuf,
    #[serde(default)]
    pub features: Vec<String>,
    pub tests: Vec<Test>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Image {
    Std,
    Freestanding,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Executor {
    Rust,
    Cpu,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Returns {
    Unit,
    Result,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    pub id: String,
    pub name: String,
    pub entry: String,
    pub returns: Returns,
    pub features: Vec<String>,
}

impl Catalog {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)
    }
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(source).context("parse board suite")?;
        ensure!(
            catalog.schema_version == 1 && identifier(&catalog.board),
            "invalid board suite identity"
        );
        let order: BTreeSet<_> = catalog.run_order.iter().collect();
        ensure!(
            !order.is_empty()
                && order.len() == catalog.run_order.len()
                && order.len() == catalog.batches.len()
                && order.iter().all(|id| catalog.batches.contains_key(*id)),
            "board run_order must contain every batch exactly once"
        );
        let mut members = BTreeSet::new();
        let mut builds = BTreeSet::new();
        for (id, batch) in &catalog.batches {
            ensure!(
                identifier(id)
                    && ["aarch64", "riscv64", "x86_64", "loongarch64"]
                        .contains(&batch.arch.as_str()),
                "invalid board batch identity or architecture"
            );
            ensure!(
                identifier(&batch.package) && !batch.tests.is_empty(),
                "board batch needs a package and members"
            );
            ensure!(
                batch.executor != Executor::Rust || batch.image == Image::Std,
                "Rust board executor requires std"
            );
            ensure!(
                builds.insert(&batch.build_config),
                "each board batch must own its build configuration"
            );
            for path in [&batch.build_config, &batch.board_config] {
                ensure!(
                    !path.as_os_str().is_empty()
                        && path
                            .components()
                            .all(|part| matches!(part, Component::Normal(_))),
                    "board configuration must stay inside its suite"
                );
            }
            for test in &batch.tests {
                ensure!(
                    identifier(&test.id)
                        && members.insert(&test.id)
                        && !test.name.trim().is_empty(),
                    "invalid or duplicate board member"
                );
                ensure!(
                    !catalog.batches.contains_key(&test.id)
                        || (&test.id == id && batch.tests.len() == 1),
                    "ambiguous board batch/member selector"
                );
                ensure!(
                    test.entry.split("::").all(|part| !part.is_empty()
                        && part
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
                        && (test.entry.starts_with("cpu_tests::")
                            || test.entry.starts_with("crate::")),
                    "invalid board entry"
                );
                ensure!(
                    batch.executor != Executor::Cpu
                        || (test.returns == Returns::Unit && test.entry.starts_with("cpu_tests::")),
                    "CPU executor requires unit-returning CPU library entries"
                );
            }
            for feature in batch
                .features
                .iter()
                .chain(batch.tests.iter().flat_map(|test| &test.features))
            {
                ensure!(
                    !feature.is_empty()
                        && feature
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || "-_/?:".contains(ch)),
                    "invalid board Cargo feature"
                );
            }
        }
        Ok(catalog)
    }
}
impl Batch {
    /// Enables the whole compatible batch even for a single-member diagnostic.
    pub fn features(&self) -> Vec<String> {
        self.features
            .iter()
            .chain(self.tests.iter().flat_map(|test| &test.features))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    /// Resolves members in declaration order; feature unification never selects calls.
    pub fn select<'a>(&'a self, id: &str, requested: &[String]) -> anyhow::Result<Vec<&'a Test>> {
        let all = requested.is_empty() || requested.iter().any(|item| item == id);
        let tests: Vec<_> = self
            .tests
            .iter()
            .filter(|test| all || requested.contains(&test.id))
            .collect();
        ensure!(
            !tests.is_empty(),
            "board selection matches no members in {id}"
        );
        Ok(tests)
    }
}
pub fn selection(selected: &[String], members: &[String]) -> String {
    if selected == members {
        "all".to_owned()
    } else {
        selected.join(",")
    }
}

pub fn completion(id: &str, selection: &str) -> String {
    format!("BOARD_BATCH_OK batch={id} selection={selection}")
}
pub fn build_env(path: &Path, id: &str, selection: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (SUITE_ENV.to_owned(), path.to_string_lossy().into_owned()),
        (BATCH_ENV.to_owned(), id.to_owned()),
        (
            CASES_ENV.to_owned(),
            if selection == "all" {
                String::new()
            } else {
                selection.to_owned()
            },
        ),
    ])
}
pub fn identifier(value: &str) -> bool {
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
board = "fixture"
run_order = ["combined"]
[batches.combined]
arch = "aarch64"
image = "std"
executor = "rust"
package = "fixture"
build_config = "combined/build.toml"
board_config = "combined/board.toml"
features = ["runtime"]
[[batches.combined.tests]]
id = "first"
name = "first capability"
entry = "cpu_tests::first::run"
returns = "unit"
features = ["extra"]
[[batches.combined.tests]]
id = "second"
name = "second capability"
entry = "crate::second::run"
returns = "result"
features = ["another"]
"#;
    #[test]
    fn member_selection_preserves_order_and_unifies_features_without_extra_calls() {
        let catalog = Catalog::parse(SOURCE).unwrap();
        let batch = &catalog.batches["combined"];
        let selected = batch.select("combined", &["second".into()]).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|test| test.id.as_str())
                .collect::<Vec<_>>(),
            ["second"]
        );
        assert_eq!(batch.features(), ["another", "extra", "runtime"]);
        assert_eq!(
            selection(
                &selected
                    .iter()
                    .map(|test| test.id.clone())
                    .collect::<Vec<_>>(),
                &batch
                    .tests
                    .iter()
                    .map(|test| test.id.clone())
                    .collect::<Vec<_>>()
            ),
            "second"
        );
        assert!(batch.select("combined", &["missing".into()]).is_err());
        let selected = batch
            .select(
                "combined",
                &["second".into(), "first".into(), "second".into()],
            )
            .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|test| test.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }
    #[test]
    fn malformed_orders_escaping_paths_and_ambiguous_members_are_rejected() {
        for source in [
            SOURCE.replace("[\"combined\"]", "[\"combined\",\"combined\"]"),
            SOURCE.replace("combined/build.toml", "../outside.toml"),
            SOURCE.replace("id = \"second\"", "id = \"first\""),
        ] {
            assert!(Catalog::parse(&source).is_err());
        }
    }
}
