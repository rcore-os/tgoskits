use super::{
    ARCEOS_AXTEST_GROUP, ARCEOS_AXTEST_RUSTFLAGS,
    assets::arceos_test_group_dir,
    discovery::{discover_qemu_cases_in_dir, discover_qemu_cases_in_dir_allow_empty},
    runner::run_prepared_qemu_groups,
    rust_qemu::prepare_rust_qemu_cases,
    types::GenericQemuRunOptions,
};
use crate::{arceos::ArceOS, build::append_encoded_rustflags};

pub(super) async fn test_axtest_qemu(
    arceos: &mut ArceOS,
    arch: &str,
    target: &str,
    options: GenericQemuRunOptions<'_>,
) -> anyhow::Result<()> {
    let dir = arceos_test_group_dir(arceos.app.workspace_root(), ARCEOS_AXTEST_GROUP);
    let cases = if options.allow_empty && options.selected_case.is_none() {
        discover_qemu_cases_in_dir_allow_empty(
            &dir,
            arch,
            target,
            options.selected_case,
            ARCEOS_AXTEST_GROUP,
        )?
    } else {
        discover_qemu_cases_in_dir(
            &dir,
            arch,
            target,
            options.selected_case,
            ARCEOS_AXTEST_GROUP,
        )?
    };
    if cases.is_empty() {
        println!("skipping arceos axtest qemu tests for arch: {arch} (target: {target}, no cases)");
        return Ok(());
    }

    println!(
        "running arceos axtest qemu tests for arch: {arch} (target: {target}, cases: {})",
        cases.len()
    );
    let mut prepared = prepare_rust_qemu_cases(arceos, target, cases).await?;
    for case in &mut prepared {
        append_encoded_rustflags(&mut case.cargo, ARCEOS_AXTEST_RUSTFLAGS);
        if crate::support::axtest_coverage::enabled(&case.cargo) {
            crate::support::axtest_coverage::prepare_cargo(&mut case.cargo);
        }
    }

    run_prepared_qemu_groups(
        arceos,
        ARCEOS_AXTEST_GROUP,
        "arceos axtest",
        &prepared,
        options.symbolize_after,
        options.keep_qemu_log,
    )
    .await
}

#[cfg(test)]
mod tests {
    use ostool::build::config::Cargo;

    use super::*;

    #[test]
    fn append_encoded_rustflags_preserves_existing_flags() {
        let mut cargo = Cargo {
            env: [(
                "CARGO_ENCODED_RUSTFLAGS".to_string(),
                "-Cdebuginfo=2".to_string(),
            )]
            .into(),
            ..Cargo::default()
        };

        append_encoded_rustflags(&mut cargo, &["--cfg", "axtest"]);

        assert_eq!(
            cargo.env.get("CARGO_ENCODED_RUSTFLAGS").map(String::as_str),
            Some("-Cdebuginfo=2\u{1f}--cfg\u{1f}axtest")
        );
    }

    #[test]
    fn append_encoded_rustflags_skips_existing_sequence() {
        let mut cargo = Cargo {
            env: [(
                "CARGO_ENCODED_RUSTFLAGS".to_string(),
                "--cfg\u{1f}axtest\u{1f}--check-cfg\u{1f}cfg(axtest)".to_string(),
            )]
            .into(),
            ..Cargo::default()
        };

        append_encoded_rustflags(
            &mut cargo,
            &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"],
        );

        assert_eq!(
            cargo.env.get("CARGO_ENCODED_RUSTFLAGS").map(String::as_str),
            Some("--cfg\u{1f}axtest\u{1f}--check-cfg\u{1f}cfg(axtest)")
        );
    }
}
