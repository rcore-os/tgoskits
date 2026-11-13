use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    schema_version: u32,
    pub run_order: Vec<String>,
    pub batches: BTreeMap<String, Batch>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Batch {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    pub tests: Vec<Test>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Test {
    pub id: String,
    pub name: String,
    pub entry: String,
    #[serde(default)]
    pub features: Vec<String>,
}
pub(super) struct Selection<'a> {
    pub id: &'a str,
    pub batch: &'a Batch,
    pub tests: Vec<&'a Test>,
}
impl Catalog {
    pub fn load(root: &Path) -> anyhow::Result<Self> {
        Self::parse(
            &fs::read_to_string(root.join("suite.toml")).context("read C suite declaration")?,
        )
    }
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(source).context("parse C suite declaration")?;
        let order: BTreeSet<_> = catalog.run_order.iter().collect();
        ensure!(
            catalog.schema_version == 1
                && !order.is_empty()
                && order.len() == catalog.run_order.len()
                && order.len() == catalog.batches.len()
                && order.iter().all(|id| catalog.batches.contains_key(*id)),
            "C run_order must contain each batch exactly once"
        );
        let mut selectors: BTreeSet<&str> = catalog.batches.keys().map(String::as_str).collect();
        for (id, batch) in &catalog.batches {
            ensure!(
                identifier(id) && !batch.name.trim().is_empty() && !batch.tests.is_empty(),
                "invalid C batch identity or empty batch"
            );
            for test in &batch.tests {
                ensure!(
                    identifier(&test.id) && selectors.insert(&test.id),
                    "duplicate or ambiguous C test selector"
                );
                ensure!(
                    !test.name.trim().is_empty() && !test.name.chars().any(char::is_control),
                    "invalid C test name"
                );
                let mut chars = test.entry.chars();
                ensure!(
                    chars
                        .next()
                        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
                    "invalid C entry identifier"
                );
            }
        }
        for batch in catalog.batches.values() {
            for alias in &batch.aliases {
                ensure!(
                    identifier(alias) && selectors.insert(alias),
                    "duplicate C alias"
                );
            }
        }
        Ok(catalog)
    }
    pub fn select(&self, requested: Option<&str>) -> anyhow::Result<Vec<Selection<'_>>> {
        let requested: Vec<_> = requested
            .map(|value| value.split(',').map(str::trim).collect())
            .unwrap_or_default();
        ensure!(
            requested.iter().all(|id| !id.is_empty()),
            "empty C test selector"
        );
        for id in &requested {
            ensure!(
                self.batches.iter().any(|(name, batch)| name == id
                    || batch.aliases.iter().any(|alias| alias == id)
                    || batch.tests.iter().any(|test| test.id == *id)),
                "unknown ArceOS C test selector `{id}`"
            );
        }
        let mut result = Vec::new();
        for id in &self.run_order {
            let batch = &self.batches[id];
            let all = requested.is_empty()
                || requested.contains(&id.as_str())
                || batch
                    .aliases
                    .iter()
                    .any(|alias| requested.contains(&alias.as_str()));
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
            "C selection must execute at least one test"
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
    pub fn label(&self) -> String {
        if self.tests.len() == self.batch.tests.len() {
            "all".into()
        } else {
            self.members().join(",")
        }
    }
    pub fn completion(&self) -> String {
        format!("C_BATCH_OK batch={} selection={}", self.id, self.label())
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
schema_version=1
run_order=["combined"]
[batches.combined]
name="combined capabilities"
aliases=["everything"]
features=["shared"]
[[batches.combined.tests]]
id="first"
name="first behavior"
entry="check_first"
features=["first-cap"]
[[batches.combined.tests]]
id="second"
name="second behavior"
entry="check_second"
features=["second-cap"]
"#;
    #[test]
    fn selection_uses_order_and_full_capabilities_without_extra_calls() {
        let catalog = Catalog::parse(SOURCE).unwrap();
        let single = catalog.select(Some("second")).unwrap();
        assert_eq!(single[0].members(), ["second"]);
        assert_eq!(single[0].features(), ["first-cap", "second-cap", "shared"]);
        let both = catalog.select(Some("second,first,second")).unwrap();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].members(), ["first", "second"]);
        assert_eq!(
            catalog.select(Some("everything")).unwrap()[0].members(),
            ["first", "second"]
        );
        for value in ["missing", "first,", ""] {
            assert!(catalog.select(Some(value)).is_err());
        }
    }
    #[test]
    fn invalid_registration_is_rejected() {
        for source in [
            SOURCE.replace("run_order=[\"combined\"]", "run_order=[]"),
            SOURCE.replace("id=\"second\"", "id=\"first\""),
            SOURCE.replace("check_second", "check_second();"),
            SOURCE.replace("everything", "first"),
        ] {
            assert!(Catalog::parse(&source).is_err());
        }
    }
}
