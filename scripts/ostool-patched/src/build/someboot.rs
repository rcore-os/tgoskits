use std::{collections::HashMap, path::PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct TargetBuildInfo {
    #[serde(default)]
    rustflags: Vec<String>,
    #[serde(default)]
    cargoargs: Vec<String>,
}

pub fn detect_build_config(manifest_path: &PathBuf, target: &str) -> anyhow::Result<Vec<String>> {
    let mut cargo_args = Vec::new();

    let meta = read_metadata(manifest_path, true)?;
    let mut someboot_roots = collect_someboot_roots(&meta);
    if someboot_roots.is_empty() {
        // `--no-deps` metadata does not include transitive crates.io packages.
        // Fall back to full metadata when no local/direct hit is found.
        let meta_with_deps = read_metadata(manifest_path, false)?;
        someboot_roots = collect_someboot_roots(&meta_with_deps);
    }

    if someboot_roots.is_empty() {
        return Ok(cargo_args);
    }

    let build_info_path = someboot_roots
        .into_iter()
        .map(|root| root.join("build-info.toml"))
        .find(|p| p.exists());

    let Some(build_info_path) = build_info_path else {
        return Ok(cargo_args);
    };

    let build_info_raw = std::fs::read_to_string(&build_info_path).with_context(|| {
        format!(
            "failed to read build-info.toml: {}",
            build_info_path.display()
        )
    })?;

    let build_info: HashMap<String, TargetBuildInfo> = toml::from_str(&build_info_raw)
        .with_context(|| {
            format!(
                "failed to parse build-info.toml at {}",
                build_info_path.display()
            )
        })?;

    let Some(matched) = pick_target_build_info(&build_info, target) else {
        return Ok(cargo_args);
    };

    cargo_args.extend(matched.cargoargs.iter().cloned());

    if !matched.rustflags.is_empty() {
        cargo_args.push("--config".to_string());
        cargo_args.push(rustflags_to_cargo_override(target, &matched.rustflags));
    }

    Ok(cargo_args)
}

pub fn detect_build_config_for_package(
    manifest_path: &PathBuf,
    package: &str,
    features: &[String],
    target: &str,
) -> anyhow::Result<Vec<String>> {
    let mut cargo_args = Vec::new();

    if !someboot_reachable_for_package(manifest_path, package, features, target)? {
        return Ok(cargo_args);
    }

    let meta = read_metadata(manifest_path, false)?;
    let someboot_roots = collect_someboot_roots(&meta);
    if someboot_roots.is_empty() {
        return Ok(cargo_args);
    }

    let build_info_path = someboot_roots
        .into_iter()
        .map(|root| root.join("build-info.toml"))
        .find(|p| p.exists());

    let Some(build_info_path) = build_info_path else {
        return Ok(cargo_args);
    };

    let build_info_raw = std::fs::read_to_string(&build_info_path).with_context(|| {
        format!(
            "failed to read build-info.toml: {}",
            build_info_path.display()
        )
    })?;

    let build_info: HashMap<String, TargetBuildInfo> = toml::from_str(&build_info_raw)
        .with_context(|| {
            format!(
                "failed to parse build-info.toml at {}",
                build_info_path.display()
            )
        })?;

    let Some(matched) = pick_target_build_info(&build_info, target) else {
        return Ok(cargo_args);
    };

    cargo_args.extend(matched.cargoargs.iter().cloned());

    if !matched.rustflags.is_empty() {
        cargo_args.push("--config".to_string());
        cargo_args.push(rustflags_to_cargo_override(target, &matched.rustflags));
    }

    Ok(cargo_args)
}

fn read_metadata(
    manifest_path: &PathBuf,
    no_deps: bool,
) -> anyhow::Result<cargo_metadata::Metadata> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    cmd.manifest_path(manifest_path);
    if no_deps {
        cmd.no_deps();
    }

    cmd.exec().with_context(|| {
        let mode = if no_deps { "--no-deps" } else { "with deps" };
        format!(
            "failed to read Cargo metadata ({mode}) from manifest path: {}",
            manifest_path.display()
        )
    })
}

fn collect_someboot_roots(meta: &cargo_metadata::Metadata) -> Vec<PathBuf> {
    let mut someboot_roots: Vec<PathBuf> = meta
        .packages
        .iter()
        .flat_map(|pkg| pkg.dependencies.iter())
        .filter(|dep| dep.name == "someboot")
        .filter_map(|dep| dep.path.clone())
        .map(|p| p.into_std_path_buf())
        .collect();

    someboot_roots.extend(
        meta.packages
            .iter()
            .filter(|pkg| pkg.name == "someboot")
            .filter_map(|pkg| {
                pkg.manifest_path
                    .parent()
                    .map(|p| p.as_std_path().to_path_buf())
            }),
    );

    someboot_roots.sort();
    someboot_roots.dedup();
    someboot_roots
}

fn someboot_reachable_for_package(
    manifest_path: &PathBuf,
    package: &str,
    features: &[String],
    target: &str,
) -> anyhow::Result<bool> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("tree");
    cmd.arg("--manifest-path");
    cmd.arg(manifest_path);
    cmd.arg("-p");
    cmd.arg(package);
    cmd.arg("--target");
    cmd.arg(target);
    cmd.arg("-e");
    cmd.arg("normal,build");
    cmd.arg("--prefix");
    cmd.arg("none");
    cmd.arg("--format");
    cmd.arg("{p}");
    if !features.is_empty() {
        cmd.arg("--features");
        cmd.arg(features.join(","));
    }

    let output = cmd.output().with_context(|| {
        format!(
            "failed to run `cargo tree` for package `{}` and target `{}`",
            package, target
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "`cargo tree` failed for package `{}` and target `{}`: {}\nstderr:\n{}",
            package,
            target,
            output.status,
            stderr
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with("someboot v")))
}

fn pick_target_build_info<'a>(
    build_info: &'a HashMap<String, TargetBuildInfo>,
    target: &str,
) -> Option<&'a TargetBuildInfo> {
    if let Some(exact) = build_info.get(target) {
        return Some(exact);
    }

    let mut contains_target: Vec<_> = build_info
        .iter()
        .filter(|(cfg_target, _)| target.contains(cfg_target.as_str()))
        .collect();

    contains_target.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
    if let Some((_, info)) = contains_target.first() {
        return Some(*info);
    }

    let mut target_contains: Vec<_> = build_info
        .iter()
        .filter(|(cfg_target, _)| cfg_target.contains(target))
        .collect();

    target_contains.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(b.0)));
    target_contains.first().map(|(_, info)| *info)
}

fn rustflags_to_cargo_override(target: &str, rustflags: &[String]) -> String {
    let rustflags_toml =
        toml::Value::Array(rustflags.iter().cloned().map(toml::Value::String).collect())
            .to_string();

    format!("target.{target}.rustflags={rustflags_toml}")
}

#[cfg(test)]
mod tests {
    use super::{detect_build_config, detect_build_config_for_package};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn test_local() {
        detect_build_config_works_for_sparreal_manifest(
            "/home/ubuntu/workspace/sparreal-os/Cargo.toml",
        );
    }

    #[test]
    fn test_crateio() {
        detect_build_config_works_for_sparreal_manifest(
            "/home/ubuntu/workspace/tgoskits/Cargo.toml",
        );
    }

    fn detect_build_config_works_for_sparreal_manifest(p: &str) {
        let manifest_path = PathBuf::from(p);
        if !manifest_path.exists() {
            return;
        }

        let args = detect_build_config(&manifest_path, "aarch64-unknown-none")
            .expect("detect_build_config should succeed");

        println!("Detected cargo args: ");
        for arg in &args {
            println!("  {arg}");
        }

        assert!(
            args.len() >= 4,
            "expected at least cargoargs and rustflags config, got: {args:?}"
        );
        assert_eq!(&args[0..2], ["-Z", "build-std=core,alloc"]);
        assert_eq!(args[2], "--config");

        let rustflags_config = args
            .iter()
            .find(|arg| arg.starts_with("target.aarch64-unknown-none.rustflags="))
            .expect("target rustflags command-line override should exist");

        assert!(rustflags_config.contains("\"-C\""));
        assert!(rustflags_config.contains("\"relocation-model=pic\""));
        assert!(rustflags_config.contains("\"-Clink-args=-pie\""));

        let contains_match_args =
            detect_build_config(&manifest_path, "aarch64-unknown-none-softfloat")
                .expect("contains match should succeed");
        assert_eq!(&contains_match_args[0..2], ["-Z", "build-std=core,alloc"]);
        assert!(
            contains_match_args
                .iter()
                .any(|arg| arg.starts_with("target.aarch64-unknown-none-softfloat.rustflags="))
        );
    }

    #[test]
    fn detect_build_config_for_package_skips_unreachable_optional_someboot() {
        let root = std::env::temp_dir().join(format!(
            "ostool-someboot-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be valid")
                .as_nanos()
        ));

        if root.exists() {
            fs::remove_dir_all(&root).expect("failed to remove old temp dir");
        }

        fs::create_dir_all(root.join("app/src")).unwrap();
        fs::create_dir_all(root.join("helper/src")).unwrap();
        fs::create_dir_all(root.join("someboot/src")).unwrap();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"helper\", \"someboot\"]\nresolver = \"2\"\n",
        )
        .unwrap();

        fs::write(
            root.join("app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n\
             helper = { path = \"../helper\", default-features = false }\n\n[features]\n\
             with-someboot = [\"helper/use-someboot\"]\n",
        )
        .unwrap();
        fs::write(root.join("app/src/main.rs"), "fn main() {}\n").unwrap();

        fs::write(
            root.join("helper/Cargo.toml"),
            "[package]\nname = \"helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\n\
             use-someboot = [\"dep:someboot\"]\n\n[dependencies]\n\
             someboot = { path = \"../someboot\", optional = true }\n",
        )
        .unwrap();
        fs::write(root.join("helper/src/lib.rs"), "pub fn helper() {}\n").unwrap();

        fs::write(
            root.join("someboot/Cargo.toml"),
            "[package]\nname = \"someboot\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(root.join("someboot/src/lib.rs"), "pub fn marker() {}\n").unwrap();
        fs::write(
            root.join("someboot/build-info.toml"),
            "[x86_64-unknown-none]\n\
             rustflags = [\"-C\", \"relocation-model=pic\"]\n\
             cargoargs = [\"-Z\", \"build-std=core,alloc\"]\n",
        )
        .unwrap();

        let manifest_path = root.join("Cargo.toml");

        let without_optional =
            detect_build_config_for_package(&manifest_path, "app", &[], "x86_64-unknown-none")
                .unwrap();
        assert!(without_optional.is_empty());

        let with_optional = detect_build_config_for_package(
            &manifest_path,
            "app",
            &["app/with-someboot".to_string()],
            "x86_64-unknown-none",
        )
        .unwrap();
        assert_eq!(&with_optional[0..2], ["-Z", "build-std=core,alloc"]);
        assert_eq!(with_optional[2], "--config");
        assert!(
            with_optional
                .iter()
                .any(|arg| arg.starts_with("target.x86_64-unknown-none.rustflags="))
        );

        fs::remove_dir_all(&root).expect("failed to remove temp workspace");
    }
}
