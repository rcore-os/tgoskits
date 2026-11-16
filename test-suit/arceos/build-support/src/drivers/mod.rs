pub mod catalog;

use std::{env, fs, path::PathBuf};
/// Generate the selected driver calls during the target build.
pub fn generate_bindings() {
    println!("cargo:rerun-if-changed=suite.toml");
    for key in [catalog::BATCH_ENV, catalog::CASES_ENV] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let suite = catalog::Catalog::load(&root).expect("valid drivers declaration");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("target arch");
    let batch = env::var(catalog::BATCH_ENV).ok();
    let members = env::var(catalog::CASES_ENV)
        .ok()
        .filter(|text| !text.is_empty());
    let selected = suite
        .select(&arch, members.as_deref().or(batch.as_deref()))
        .expect("valid drivers selection");
    assert_eq!(selected.len(), 1, "one batch per driver image");
    assert_eq!(
        selected[0].batch.package,
        env::var("CARGO_PKG_NAME").expect("package")
    );
    assert_eq!(selected[0].batch.image, "freestanding");
    assert_eq!(selected[0].batch.mode, "normal");
    if let Some(batch) = batch {
        assert_eq!(selected[0].id, batch, "batch and member selection disagree");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(output.join("selected_drivers.rs"), generate(&selected[0]))
        .expect("write driver calls");
}

fn generate(selection: &catalog::Selection<'_>) -> String {
    let mut code = String::from("fn run_selected_batch() {\n");
    for test in &selection.tests {
        code.push_str(&format!(
            "std::println!({:?});\nlet () = {}();\nstd::println!({:?});\n",
            format!("DRIVER_TEST_BEGIN {}", test.id),
            test.entry,
            format!("DRIVER_TEST_PASS {}", test.id)
        ));
    }
    code.push_str(&format!(
        "std::println!({:?});\n}}\n",
        selection.completion()
    ));
    code
}
