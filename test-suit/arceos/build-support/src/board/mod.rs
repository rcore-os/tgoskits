use std::path::PathBuf;

use anyhow::{Context, ensure};

pub mod catalog;

pub use catalog::{Executor, Image};

use self::catalog::{BATCH_ENV, CASES_ENV, Catalog, Returns, SUITE_ENV, completion, selection};
/// Generates target calls from the same declaration used by host selection.
/// Build scripts call this before their ordinary QEMU-only generation path.
pub fn generate_bindings(executor: Executor, image: Image) -> anyhow::Result<Option<String>> {
    use std::env;
    for key in [SUITE_ENV, BATCH_ENV, CASES_ENV] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let Some(path) = env::var_os(SUITE_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    println!("cargo:rerun-if-changed={}", path.display());
    let catalog = Catalog::load(&path)?;
    let id = env::var(BATCH_ENV).context("board batch must be selected")?;
    let batch = catalog.batches.get(&id).context("unknown board batch")?;
    ensure!(
        batch.executor == executor
            && batch.image == image
            && batch.package == env::var("CARGO_PKG_NAME")?,
        "board executor, image or package mismatch"
    );
    ensure!(
        batch.arch == env::var("CARGO_CFG_TARGET_ARCH")?,
        "board architecture mismatch"
    );
    let requested: Vec<_> = env::var(CASES_ENV)
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect();
    ensure!(
        requested
            .iter()
            .all(|id| batch.tests.iter().any(|test| &test.id == id)),
        "unknown board member selection"
    );
    let tests = batch.select(&id, &requested)?;
    let complete = completion(
        &id,
        &selection(
            &tests.iter().map(|test| test.id.clone()).collect::<Vec<_>>(),
            &batch
                .tests
                .iter()
                .map(|test| test.id.clone())
                .collect::<Vec<_>>(),
        ),
    );
    let mut code = String::new();
    match executor {
        Executor::Rust => {
            code.push_str("register_tests! {\n");
            for test in &tests {
                let name = test.id.replace('-', "_");
                let body = match test.returns {
                    Returns::Unit => format!("let () = {}(); Ok(())", test.entry),
                    Returns::Result => format!("{}()", test.entry),
                };
                code.push_str(&format!(
                    "({:?}, {:?}, run_board_{name}, {{ {body} }});\n",
                    test.id, test.name
                ));
            }
            code.push_str("}\n");
            code.push_str(&format!(
                "pub const BOARD_COMPLETION: Option<&str> = Some({complete:?});\n"
            ));
        }
        Executor::Cpu => {
            code.push_str("fn run_selected_batch() {\n");
            for test in tests {
                code.push_str(&format!(
                    "std::println!({:?});\nlet () = {}();\nstd::println!({:?});\n",
                    format!("CPU_TEST_BEGIN {}", test.id),
                    test.entry,
                    format!("CPU_TEST_PASS {}", test.id)
                ));
            }
            code.push_str(&format!("std::println!({complete:?});\n}}\n"));
        }
    }
    Ok(Some(code))
}
