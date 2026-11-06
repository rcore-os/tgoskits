use super::*;

pub(super) fn prepare_ktest_cargo(
    cargo: &mut Cargo,
    target: &KtestTarget,
    runtime: KtestRuntime,
    coverage: bool,
) {
    cargo.bin = None;
    cargo.test = Some(target.name.clone());
    remove_cargo_target_selector_args(&mut cargo.args);
    cargo.env.remove("AXBUILD_STARRY_BIN");
    // `ktest` is an explicit command: the harness feature and the target's
    // declared required features are the only additions made here.
    ensure_feature(cargo, AXTEST_FEATURE);
    for feature in &target.required_features {
        ensure_feature(cargo, feature);
    }
    if matches!(runtime, KtestRuntime::Arceos | KtestRuntime::Board) {
        ensure_feature(cargo, "ax-std/arceos");
    }
    crate::build::append_cargo_rustflags(cargo, AXTEST_RUSTFLAGS);
    if coverage {
        cargo
            .env
            .insert("AXTEST_COVERAGE".to_string(), "y".to_string());
        crate::support::axtest_coverage::prepare_cargo(cargo);
    } else {
        // Coverage is owned by the explicit ktest CLI flag. Package-local
        // build configuration must not turn it on implicitly.
        cargo.env.remove("AXTEST_COVERAGE");
    }
}


pub(super) fn ensure_feature(cargo: &mut Cargo, feature: &str) {
    if !cargo.features.iter().any(|candidate| candidate == feature) {
        cargo.features.push(feature.to_string());
    }
}


pub(super) fn apply_qemu_cargo_options(cargo: &mut Cargo, args: &ArgsKtestQemu) {
    for feature in &args.features {
        ensure_feature(cargo, feature);
    }
    if args.all_features {
        ensure_cargo_arg(&mut cargo.args, "--all-features");
    }
    if args.no_default_features {
        ensure_cargo_arg(&mut cargo.args, "--no-default-features");
    }
    if args.locked {
        ensure_cargo_arg(&mut cargo.args, "--locked");
    }
    if args.offline {
        ensure_cargo_arg(&mut cargo.args, "--offline");
    }
    if args.frozen {
        ensure_cargo_arg(&mut cargo.args, "--frozen");
    }
    if let Some(profile) = &args.profile {
        remove_cargo_value_arg(&mut cargo.args, "--profile");
        cargo.profile = Some(if profile == "release" {
            CargoBuildProfile::Release
        } else {
            CargoBuildProfile::Debug
        });
        if !matches!(profile.as_str(), "dev" | "debug" | "release") {
            cargo
                .args
                .extend(["--profile".to_string(), profile.clone()]);
        }
    }
}


pub(super) fn ensure_cargo_arg(args: &mut Vec<String>, arg: &str) {
    if !args.iter().any(|candidate| candidate == arg) {
        args.push(arg.to_string());
    }
}


pub(super) fn remove_cargo_value_arg(args: &mut Vec<String>, name: &str) {
    let equals_prefix = format!("{name}=");
    let mut filtered = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == name {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with(&equals_prefix) {
            continue;
        }
        filtered.push(arg.clone());
    }
    *args = filtered;
}


pub(super) fn remove_cargo_target_selector_args(args: &mut Vec<String>) {
    let mut filtered = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if matches!(arg.as_str(), "--bin" | "--test") {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--bin=") || arg.starts_with("--test=") {
            continue;
        }
        filtered.push(arg.clone());
    }
    *args = filtered;
}

