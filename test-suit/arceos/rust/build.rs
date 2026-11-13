use std::{env, fs, path::PathBuf};

use arceos_test_build::{board as board_bindings, board::catalog as board_catalog};

fn strings(value: Option<&toml::Value>) -> Vec<&str> {
    value
        .into_iter()
        .flat_map(|value| value.as_array().expect("suite list must be an array"))
        .map(|value| value.as_str().expect("suite list must contain strings"))
        .collect()
}

fn main() {
    if let Some(generated) =
        board_bindings::generate_bindings(board_catalog::Executor::Rust, board_catalog::Image::Std)
            .expect("valid board suite bindings")
    {
        let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
        fs::write(output.join("suite_bindings.rs"), generated).expect("write board suite bindings");
        return;
    }
    println!("cargo:rerun-if-changed=suite.toml");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let source = fs::read_to_string("suite.toml").expect("failed to read suite declaration");
    let suite: toml::Value = toml::from_str(&source).expect("invalid suite declaration");
    assert_eq!(
        suite["schema_version"].as_integer(),
        Some(3),
        "unsupported suite schema"
    );
    let batches = suite["batches"]
        .as_table()
        .expect("suite batches must be named tables");
    let run_order = strings(suite.get("run_order"));
    assert!(
        !run_order.is_empty(),
        "suite declaration contains no run_order"
    );
    assert_eq!(
        run_order.len(),
        batches.len(),
        "run_order must include every batch exactly once"
    );
    let mut generated = String::from("register_tests! {\n");
    let mut ids = std::collections::BTreeSet::new();
    let manifest: toml::Value =
        toml::from_str(&fs::read_to_string("Cargo.toml").expect("failed to read suite manifest"))
            .expect("invalid suite manifest");
    let mut batch_ids = std::collections::BTreeSet::new();
    for batch_id in run_order {
        assert!(
            batch_ids.insert(batch_id),
            "duplicate batch id {batch_id} in run_order"
        );
        let batch = batches
            .get(batch_id)
            .unwrap_or_else(|| panic!("unknown batch {batch_id} in run_order"));
        let tests = batch["tests"]
            .as_array()
            .expect("batch tests must be an array");
        assert!(!tests.is_empty(), "batch {batch_id} contains no tests");
        let batch_arches = strings(batch.get("arches"));
        for test in tests {
            let id = test["id"].as_str().expect("test id must be a string");
            assert!(ids.insert(id), "duplicate test id {id}");
            assert!(
                !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            );
            assert!(
                manifest["features"].get(id).is_some(),
                "test {id} has no Cargo feature"
            );
            let name = test["name"].as_str().expect("test name must be a string");
            let entry = test["entry"].as_str().expect("test entry must be a path");
            assert!(
                entry.split("::").all(|part| !part.is_empty()
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
                "invalid entry path"
            );
            generated.push_str(&format!("    #[cfg(feature = {id:?})]\n"));
            let test_arches = strings(test.get("arches"));
            assert!(
                test_arches
                    .iter()
                    .all(|arch| batch_arches.is_empty() || batch_arches.contains(arch)),
                "test {id} exceeds its batch architecture support"
            );
            let arches = if test_arches.is_empty() {
                &batch_arches
            } else {
                &test_arches
            };
            if !arches.is_empty() {
                let values: Vec<_> = arches
                    .iter()
                    .map(|arch| format!("target_arch = {arch:?}"))
                    .collect();
                generated.push_str(&format!("    #[cfg(any({}))]\n", values.join(",")));
            }
            let runner = format!("run_{}", id.replace('-', "_"));
            generated.push_str(&format!(
                "    ({id:?}, {name:?}, {runner}, {{ {entry}() }});\n"
            ));
        }
    }
    generated.push_str("}\npub const BOARD_COMPLETION: Option<&str> = None;\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    fs::write(output.join("suite_bindings.rs"), generated).expect("failed to write suite bindings");
}
