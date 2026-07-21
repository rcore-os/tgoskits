use std::{collections::BTreeMap, env, path::Path, process::Command};

const STARRY_TARGETS: &[&str] = &[
    "x86_64-unknown-none",
    "aarch64-unknown-none-softfloat",
    "riscv64gc-unknown-none-elf",
    "loongarch64-unknown-none-softfloat",
];

const AXVISOR_TARGETS: &[&str] = &[
    "scripts/targets/std/pie/x86_64-unknown-linux-musl.json",
    "scripts/targets/std/pie/aarch64-unknown-linux-musl.json",
    "scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json",
    "scripts/targets/std/pie/loongarch64-unknown-linux-musl.json",
];

const TLS_CHAIN: &[&str] = &[
    "ax-std",
    "ax-runtime",
    "ax-hal",
    "cpu-local",
    "axplat-dyn",
    "somehal",
    "someboot",
];

#[test]
fn axvisor_final_images_select_the_complete_unikernel_tls_chain() {
    let workspace = workspace_root();

    for target in AXVISOR_TARGETS {
        let feature_graph = resolved_features(&workspace, "axvisor", target);
        for package in TLS_CHAIN {
            assert_has_feature(&feature_graph, package, "tls", "Axvisor", target);
        }
    }
}

#[test]
fn starry_final_images_leave_kernel_tls_disabled() {
    let workspace = workspace_root();

    for target in STARRY_TARGETS {
        let feature_graph = resolved_features(&workspace, "starryos", target);
        for package in TLS_CHAIN {
            assert_lacks_feature(&feature_graph, package, "tls", "StarryOS", target);
        }
    }
}

#[test]
fn non_aarch64_final_images_never_select_arm_el2() {
    let workspace = workspace_root();

    for (package, targets) in [("axvisor", AXVISOR_TARGETS), ("starryos", STARRY_TARGETS)] {
        for target in targets.iter().filter(|target| !target.contains("aarch64-")) {
            let feature_graph = resolved_features(&workspace, package, target);
            assert_lacks_feature(&feature_graph, "ax-cpu", "arm-el2", package, target);
        }
    }
}

#[test]
fn aarch64_axvisor_selects_the_el2_cpu_execution_contract() {
    let workspace = workspace_root();
    let target = "scripts/targets/std/pie/aarch64-unknown-linux-musl.json";
    let feature_graph = resolved_features(&workspace, "axvisor", target);

    assert_has_feature(&feature_graph, "ax-cpu", "arm-el2", "Axvisor", target);
}

fn resolved_features(
    workspace: &Path,
    image_package: &str,
    target: &str,
) -> BTreeMap<String, Vec<String>> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(workspace).args([
        "tree",
        "--quiet",
        "-p",
        image_package,
        "--target",
        target,
        "-e",
        "normal,build",
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
    ]);
    if target.ends_with(".json") {
        command.args(["-Z", "json-target-spec"]);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to inspect {image_package}/{target}: {error}"));
    assert!(
        output.status.success(),
        "failed to inspect {image_package}/{target}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8");
    let mut graph = BTreeMap::<String, Vec<String>>::new();
    for line in stdout.lines() {
        let Some((package, features)) = line.split_once('|') else {
            continue;
        };
        let Some((package_name, _)) = package.split_once(" v") else {
            continue;
        };
        let package_features = graph.entry(package_name.to_owned()).or_default();
        package_features.extend(
            features
                .trim_end_matches(" (*)")
                .split(',')
                .filter(|feature| !feature.is_empty())
                .map(str::to_owned),
        );
        package_features.sort_unstable();
        package_features.dedup();
    }
    graph
}

fn assert_has_feature(
    graph: &BTreeMap<String, Vec<String>>,
    package: &str,
    feature: &str,
    image: &str,
    target: &str,
) {
    let features = graph
        .get(package)
        .unwrap_or_else(|| panic!("{image}/{target} must resolve {package}"));
    assert!(
        features.iter().any(|resolved| resolved == feature),
        "{image}/{target} must select {package}/{feature}; resolved features: {features:?}"
    );
}

fn assert_lacks_feature(
    graph: &BTreeMap<String, Vec<String>>,
    package: &str,
    feature: &str,
    image: &str,
    target: &str,
) {
    let features = graph
        .get(package)
        .unwrap_or_else(|| panic!("{image}/{target} must resolve {package}"));
    assert!(
        features.iter().all(|resolved| resolved != feature),
        "{image}/{target} must not select {package}/{feature}; resolved features: {features:?}"
    );
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cpu-local must be nested under the workspace components directory")
        .to_path_buf()
}
