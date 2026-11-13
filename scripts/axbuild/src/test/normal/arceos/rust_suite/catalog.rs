//! One ordered declaration owns both startup batches and their target tests.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TestMetadata {
    #[serde(rename = "id")]
    pub feature: String,
    pub name: String,
    pub entry: String,
    pub category: String,
    #[serde(default)]
    pub arches: Vec<String>,
}
impl TestMetadata {
    pub fn supports_arch(&self, arch: &str) -> bool {
        supports_arch(&self.arches, arch)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BatchMetadata {
    pub id: String,
    pub arches: Vec<String>,
    pub runtime: Option<String>,
    pub tests: Vec<TestMetadata>,
}
impl BatchMetadata {
    pub fn supports_arch(&self, arch: &str) -> bool {
        supports_arch(&self.arches, arch)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteDeclaration {
    schema_version: u32,
    run_order: Vec<String>,
    batches: BTreeMap<String, BatchDeclaration>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchDeclaration {
    #[serde(default)]
    arches: Vec<String>,
    runtime: Option<String>,
    tests: Vec<TestMetadata>,
}

/// Validated batches in explicit startup order, independent of TOML table order.
#[derive(Debug)]
pub(crate) struct SuiteCatalog {
    pub batches: Vec<BatchMetadata>,
}
impl SuiteCatalog {
    pub fn tests(&self) -> impl Iterator<Item = &TestMetadata> {
        self.batches.iter().flat_map(|batch| &batch.tests)
    }

    pub fn batch(&self, id: &str) -> Option<&BatchMetadata> {
        self.batches.iter().find(|batch| batch.id == id)
    }

    pub fn supports_selection(&self, id: &str, arch: &str) -> bool {
        !self.features_for(id, arch).is_empty()
    }

    pub fn features_for(&self, id: &str, arch: &str) -> Vec<String> {
        self.batches
            .iter()
            .filter(|batch| batch.supports_arch(arch))
            .flat_map(|batch| {
                batch.tests.iter().filter(move |test| {
                    (batch.id == id || test.feature == id) && test.supports_arch(arch)
                })
            })
            .map(|test| test.feature.clone())
            .collect()
    }

    pub fn runtime_for<'a>(&'a self, id: &'a str) -> &'a str {
        self.batch(id)
            .or_else(|| {
                self.batches
                    .iter()
                    .find(|batch| batch.tests.iter().any(|test| test.feature == id))
            })
            .and_then(|batch| batch.runtime.as_deref())
            .unwrap_or(id)
    }

    pub fn supports_test(&self, id: &str, arch: &str) -> bool {
        self.batches.iter().any(|batch| {
            batch.supports_arch(arch)
                && batch
                    .tests
                    .iter()
                    .any(|test| test.feature == id && test.supports_arch(arch))
        })
    }
}

fn supports_arch(arches: &[String], arch: &str) -> bool {
    arches.is_empty() || arches.iter().any(|candidate| candidate == arch)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn validate_arches(arches: &[String]) -> anyhow::Result<()> {
    let mut unique = BTreeSet::new();
    for arch in arches {
        if !matches!(
            arch.as_str(),
            "aarch64" | "riscv64" | "x86_64" | "loongarch64"
        ) || !unique.insert(arch)
        {
            bail!("unknown or duplicate suite architecture `{arch}`");
        }
    }
    Ok(())
}

pub(crate) fn load_catalog() -> anyhow::Result<SuiteCatalog> {
    let path = crate::context::workspace_root_path()?.join("test-suit/arceos/rust/suite.toml");
    parse_catalog(
        &fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
}

pub(super) fn parse_catalog(source: &str) -> anyhow::Result<SuiteCatalog> {
    let mut declaration: SuiteDeclaration =
        toml::from_str(source).context("invalid ordered suite metadata")?;
    if declaration.schema_version != 3 || declaration.run_order.is_empty() {
        bail!("suite requires schema_version=3 and nonempty run_order");
    }
    let mut batches = Vec::with_capacity(declaration.run_order.len());
    let mut seen = BTreeSet::new();
    for id in declaration.run_order {
        if !seen.insert(id.clone()) {
            bail!("duplicate batch `{id}` in run_order");
        }
        let definition = declaration
            .batches
            .remove(&id)
            .with_context(|| format!("unknown batch `{id}` in run_order"))?;
        batches.push(BatchMetadata {
            id,
            arches: definition.arches,
            runtime: definition.runtime,
            tests: definition.tests,
        });
    }
    if !declaration.batches.is_empty() {
        bail!(
            "batches missing from run_order: {}",
            declaration
                .batches
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let catalog = SuiteCatalog { batches };
    let mut ids = BTreeSet::new();
    let mut batches = BTreeSet::new();
    for batch in &catalog.batches {
        if !valid_id(&batch.id) || !batches.insert(&batch.id) || batch.tests.is_empty() {
            bail!("invalid, duplicate or empty execution batch `{}`", batch.id);
        }
        validate_arches(&batch.arches)?;
        if batch.runtime.as_deref().is_some_and(|name| !valid_id(name)) {
            bail!("invalid runtime fixture for batch `{}`", batch.id);
        }
        for test in &batch.tests {
            if !valid_id(&test.feature) || !ids.insert(&test.feature) {
                bail!("invalid or duplicate suite test `{}`", test.feature);
            }
            syn::parse_str::<syn::Path>(&test.entry).context("invalid target entry path")?;
            validate_arches(&test.arches)?;
            if test.arches.iter().any(|arch| !batch.supports_arch(arch)) {
                bail!(
                    "test `{}` exceeds its batch architecture support",
                    test.feature
                );
            }
        }
    }
    for batch in &catalog.batches {
        if ids.contains(&batch.id)
            && !(batch.tests.len() == 1 && batch.tests[0].feature == batch.id)
        {
            bail!("batch `{}` shadows a different test selection", batch.id);
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARATION: &str = r#"schema_version=3
run_order=['shared','isolated']
[batches.isolated]
arches=['aarch64']
[[batches.isolated.tests]]
id='third'
name='third operation'
entry='crate::example::third'
category='example'
[batches.shared]
runtime='device-fixture'
[[batches.shared.tests]]
id='first'
name='first operation'
entry='crate::example::first'
category='example'
[[batches.shared.tests]]
id='second'
name='second operation'
entry='crate::example::second'
category='example'
arches=['riscv64']
"#;

    #[test]
    fn nested_selection_preserves_order_and_inherits_runtime_and_architecture() {
        let catalog = parse_catalog(DECLARATION).unwrap();
        assert_eq!(
            catalog
                .batches
                .iter()
                .map(|batch| batch.id.as_str())
                .collect::<Vec<_>>(),
            ["shared", "isolated"]
        );
        assert_eq!(
            catalog
                .tests()
                .map(|test| test.feature.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
        let reversed = parse_catalog(&DECLARATION.replace(
            "run_order=['shared','isolated']",
            "run_order=['isolated','shared']",
        ))
        .unwrap();
        assert_eq!(
            reversed
                .tests()
                .map(|test| test.feature.as_str())
                .collect::<Vec<_>>(),
            ["third", "first", "second"]
        );
        assert_eq!(
            catalog.features_for("shared", "riscv64"),
            ["first", "second"]
        );
        assert_eq!(catalog.features_for("shared", "x86_64"), ["first"]);
        assert_eq!(catalog.features_for("second", "riscv64"), ["second"]);
        assert_eq!(catalog.runtime_for("second"), "device-fixture");
        assert_eq!(catalog.runtime_for("shared"), "device-fixture");
        assert!(catalog.supports_selection("third", "aarch64"));
        assert!(!catalog.supports_selection("third", "riscv64"));
        assert!(!catalog.supports_selection("isolated", "riscv64"));
        assert!(!catalog.supports_selection("missing", "riscv64"));
    }

    #[test]
    fn invalid_declarations_cannot_silently_duplicate_or_drop_tests() {
        for (old, new) in [
            ("schema_version=3", "schema_version=2"),
            ("id='second'", "id='first'"),
            (
                "run_order=['shared','isolated']",
                "run_order=['shared','shared']",
            ),
            (
                "run_order=['shared','isolated']",
                "run_order=['shared','missing']",
            ),
            ("run_order=['shared','isolated']", "run_order=['shared']"),
            ("id='second'", "id='shared'"),
            ("id='second'", "id='../second'"),
            ("arches=['riscv64']", "arches=['unknown']"),
            ("runtime='device-fixture'", "runtime='../device-fixture'"),
        ] {
            assert!(
                parse_catalog(&DECLARATION.replace(old, new)).is_err(),
                "{new}"
            );
        }
        assert!(
            parse_catalog("schema_version=3\nrun_order=['empty']\n[batches.empty]\ntests=[]")
                .is_err()
        );
        assert!(
            parse_catalog(&DECLARATION.replace("id='third'", "id='third'\narches=['x86_64']"))
                .is_err()
        );
    }
}
