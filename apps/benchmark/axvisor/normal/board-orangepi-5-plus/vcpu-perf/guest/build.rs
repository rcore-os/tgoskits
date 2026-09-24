use std::{env, fs, path::PathBuf};

fn main() {
    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../baseline.toml");
    println!("cargo:rerun-if-changed={}", path.display());
    let config: toml::Table = fs::read_to_string(&path)
        .expect("read benchmark baseline")
        .parse()
        .expect("parse benchmark baseline");
    let number = |key: &str| {
        config
            .get(key)
            .and_then(|value| {
                value
                    .as_float()
                    .or_else(|| value.as_integer().map(|integer| integer as f64))
            })
            .filter(|value| value.is_finite())
            .unwrap_or_else(|| panic!("baseline {key} must be a finite number"))
    };
    let baseline = number("baseline_blocks_per_second");
    let regression = number("max_regression_percent");
    let wakes = number("min_timer_wakes_per_second");
    assert!(
        baseline > 0.0 && regression > 0.0 && regression < 100.0 && wakes > 0.0,
        "invalid benchmark baseline or regression/wakeup budget"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("baseline.rs");
    fs::write(
        output,
        format!(
            "const BASELINE: f64 = {baseline:?};\nconst MAX_REGRESSION_PERCENT: f64 = \
             {regression:?};\nconst MIN_WAKE_RATE: f64 = {wakes:?};\n"
        ),
    )
    .expect("write benchmark constants");
}
