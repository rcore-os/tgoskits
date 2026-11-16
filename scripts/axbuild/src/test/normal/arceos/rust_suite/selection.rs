use anyhow::bail;

use super::catalog::SuiteCatalog;

pub(crate) fn rust_qemu_features_for_run(
    catalog: &SuiteCatalog,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<String>> {
    match selected_case {
        Some(_) => rust_qemu_features_for_list(catalog, selected_case, allow_missing_selected_case),
        None => Ok(catalog
            .batches
            .iter()
            .map(|batch| batch.id.clone())
            .collect()),
    }
}

pub(crate) fn rust_qemu_features_for_list(
    catalog: &SuiteCatalog,
    selected_case: Option<&str>,
    allow_missing_selected_case: bool,
) -> anyhow::Result<Vec<String>> {
    let mut selectors: Vec<_> = catalog
        .batches
        .iter()
        .filter(|batch| !catalog.tests().any(|test| test.feature == batch.id))
        .map(|batch| batch.id.clone())
        .collect();
    selectors.extend(catalog.tests().map(|test| test.feature.clone()));
    let Some(selected_case) = selected_case else {
        return Ok(selectors);
    };
    selectors.retain(|feature| feature == selected_case);
    if selectors.is_empty() && !allow_missing_selected_case {
        bail!("unknown ArceOS rust qemu test feature or batch `{selected_case}`");
    }
    Ok(selectors)
}

/// Shows startup boundaries in run_order; leaf names remain selectors.
pub(crate) fn render_listing(
    catalog: &SuiteCatalog,
    arch: Option<&str>,
    selectors: &[String],
) -> String {
    let batches: Vec<_> = catalog
        .batches
        .iter()
        .filter_map(|batch| {
            if arch.is_some_and(|arch| !batch.supports_arch(arch)) {
                return None;
            }
            let selected_batch = selectors.contains(&batch.id);
            let tests: Vec<_> = batch
                .tests
                .iter()
                .filter(|test| {
                    (selected_batch || selectors.contains(&test.feature))
                        && arch.is_none_or(|arch| test.supports_arch(arch))
                })
                .collect();
            (!tests.is_empty()).then_some((batch, tests))
        })
        .collect();
    let mut lines = vec!["rust".to_owned()];
    for (index, (batch, tests)) in batches.iter().enumerate() {
        let last = index + 1 == batches.len();
        let branch = if last { "└── " } else { "├── " };
        lines.push(format!(
            "{branch}{} [batch: one boot, {} test(s)]",
            batch.id,
            tests.len()
        ));
        let indent = if last { "    " } else { "│   " };
        for (index, test) in tests.iter().enumerate() {
            let branch = if index + 1 == tests.len() {
                "└── "
            } else {
                "├── "
            };
            lines.push(format!("{indent}{branch}{}", test.feature));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{super::catalog::parse_catalog, *};

    #[test]
    fn discovery_and_listing_distinguish_batch_boundaries_from_single_test_selection() {
        let catalog = parse_catalog(
            r#"schema_version=3
run_order=['shared','isolated']
[batches.shared]
[[batches.shared.tests]]
id='first'
name='operation'
entry='crate::example::run'
category='example'
[batches.isolated]
arches=['aarch64']
[[batches.isolated.tests]]
id='second'
name='operation'
entry='crate::example::run'
category='example'
"#,
        )
        .unwrap();
        assert_eq!(
            rust_qemu_features_for_run(&catalog, None, false).unwrap(),
            ["shared", "isolated"]
        );
        let selectors = rust_qemu_features_for_list(&catalog, None, false).unwrap();
        assert!(rust_qemu_features_for_run(&catalog, Some("missing"), false).is_err());
        assert!(
            rust_qemu_features_for_run(&catalog, Some("missing"), true)
                .unwrap()
                .is_empty()
        );
        let listing = render_listing(&catalog, Some("riscv64"), &selectors);
        assert_eq!(
            listing,
            "rust\n└── shared [batch: one boot, 1 test(s)]\n    └── first"
        );
        let selected = rust_qemu_features_for_list(&catalog, Some("second"), false).unwrap();
        assert_eq!(selected, ["second"]);
        assert!(render_listing(&catalog, Some("aarch64"), &selected).contains("isolated [batch:"));
    }
}
