use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .parent()
        .expect("normal image directory")
        .to_path_buf();
    let path = root.join("suite.toml");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed=AX_CPU_TEST_BATCH");
    println!("cargo:rerun-if-env-changed=AX_CPU_TEST_CASE");
    let suite: toml::Value =
        toml::from_str(&fs::read_to_string(&path).expect("CPU suite declaration"))
            .expect("valid CPU suite TOML");
    assert_eq!(suite["schema_version"].as_integer(), Some(1));
    let batches = suite["batches"].as_table().expect("CPU batches");
    let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("target architecture");
    let image = if env::var("CARGO_PKG_NAME")
        .expect("entry package")
        .ends_with("-freestanding")
    {
        "freestanding"
    } else {
        "std"
    };
    let id = env::var("AX_CPU_TEST_BATCH")
        .expect("select a CPU batch with cargo xtask arceos test qemu --test-group cpu");
    let batch = batches.get(&id).expect("selected CPU batch exists");
    assert_eq!(
        batch["arch"].as_str(),
        Some(arch.as_str()),
        "CPU batch architecture mismatch"
    );
    assert_eq!(
        batch["image"].as_str(),
        Some(image),
        "CPU batch runtime mismatch"
    );
    let selected = env::var("AX_CPU_TEST_CASE").unwrap_or_default();
    let tests: Vec<_> = batch["tests"]
        .as_array()
        .expect("CPU batch members")
        .iter()
        .filter(|test| selected.is_empty() || test["id"].as_str() == Some(&selected))
        .collect();
    assert!(
        !tests.is_empty(),
        "CPU selection must execute at least one test"
    );
    let expected_fault = batch["mode"].as_str() == Some("expected-fault");
    let mut generated = String::from("fn run_selected_batch() {\n");
    if batch.get("pin_to_boot_cpu").and_then(toml::Value::as_bool) == Some(true) {
        generated.push_str("    cpu_tests::pin_to_boot_cpu();\n");
    }
    for test in tests {
        let test_id = test["id"].as_str().expect("CPU member id");
        let entry = test["entry"].as_str().expect("CPU test entry");
        assert!(
            entry.starts_with("cpu_tests::")
                && entry.split("::").all(|part| !part.is_empty()
                    && part
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
        );
        generated.push_str(&format!(
            "    std::println!({:?});\n    let () = {entry}();\n",
            format!("CPU_TEST_BEGIN {test_id}"),
        ));
        if !expected_fault {
            generated.push_str(&format!(
                "    std::println!({:?});\n",
                format!("CPU_TEST_PASS {test_id}")
            ));
        }
    }
    if expected_fault {
        generated.push_str("    panic!(\"expected CPU diagnostic did not occur\");\n");
    } else {
        generated.push_str(&format!(
            "    std::println!({:?});\n",
            format!(
                "CPU_BATCH_OK batch={id} selection={}",
                if selected.is_empty() {
                    "all"
                } else {
                    &selected
                }
            )
        ));
    }
    generated.push_str("}\n");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(output.join("selected_batch.rs"), generated).expect("write selected CPU calls");
}
