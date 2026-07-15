use std::{collections::BTreeSet, env, path::Path, process::Command};

const AARCH64: &str = "aarch64-unknown-none-softfloat";

struct Image<'a> {
    package: &'a str,
    target: &'a str,
    features: &'a [&'a str],
    no_default_features: bool,
}

#[test]
fn arm_el2_is_owned_only_by_the_aarch64_axvisor_final_image() {
    let workspace = workspace_root();
    let aarch64_axvisor = Image {
        package: "axvisor",
        target: AARCH64,
        features: &[],
        no_default_features: false,
    };
    assert!(
        ax_cpu_features(&workspace, &aarch64_axvisor).contains("arm-el2"),
        "the AArch64 Axvisor final image must select ax-cpu/arm-el2"
    );

    let images_without_el2 = [
        Image {
            package: "axvisor",
            target: "riscv64gc-unknown-none-elf",
            features: &[],
            no_default_features: false,
        },
        Image {
            package: "axvisor",
            target: "loongarch64-unknown-none-softfloat",
            features: &[],
            no_default_features: false,
        },
        Image {
            package: "axvisor",
            target: "x86_64-unknown-none",
            features: &[],
            no_default_features: false,
        },
        Image {
            package: "ax-hal",
            target: AARCH64,
            features: &["hv"],
            no_default_features: true,
        },
        Image {
            package: "axplat-dyn",
            target: AARCH64,
            features: &["hv"],
            no_default_features: true,
        },
        Image {
            package: "arceos-helloworld",
            target: AARCH64,
            features: &["arceos"],
            no_default_features: false,
        },
    ];

    for image in images_without_el2 {
        assert!(
            !ax_cpu_features(&workspace, &image).contains("arm-el2"),
            "{}/{} must not select ax-cpu/arm-el2",
            image.package,
            image.target
        );
    }
}

fn ax_cpu_features(workspace: &Path, image: &Image<'_>) -> BTreeSet<String> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(workspace).args([
        "tree",
        "--quiet",
        "-p",
        image.package,
        "--target",
        image.target,
        "-e",
        "normal,build",
        "--prefix",
        "none",
        "--format",
        "{p}|{f}",
    ]);
    if image.no_default_features {
        command.arg("--no-default-features");
    }
    if !image.features.is_empty() {
        command.args(["--features", &image.features.join(",")]);
    }

    let output = command.output().unwrap_or_else(|error| {
        panic!(
            "failed to inspect {}/{}: {error}",
            image.package, image.target
        )
    });
    assert!(
        output.status.success(),
        "failed to inspect {}/{}:\n{}",
        image.package,
        image.target,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8");
    let mut found_ax_cpu = false;
    let mut features = BTreeSet::new();
    for line in stdout.lines() {
        let Some((package, package_features)) = line.split_once('|') else {
            continue;
        };
        if !package.starts_with("ax-cpu v") {
            continue;
        }
        found_ax_cpu = true;
        features.extend(
            package_features
                .split(',')
                .filter(|feature| !feature.is_empty())
                .map(str::to_owned),
        );
    }
    assert!(
        found_ax_cpu,
        "{}/{} must resolve ax-cpu so its register mode can be audited",
        image.package, image.target
    );
    features
}

fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ax-cpu-local must be nested under the workspace components directory")
        .to_path_buf()
}
