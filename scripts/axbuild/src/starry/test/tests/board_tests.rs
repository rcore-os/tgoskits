use super::*;

/// Reproducibility regression for the RK3588 `perf-validate` board cases.
///
/// The board runner deploys only the kernel; a userspace validator that lives
/// on the board's persistent rootfs is not built, uploaded, or version-checked
/// by the case, so a clean runner reports `not found` and a pre-staged runner
/// can silently execute a stale binary that no longer matches the committed
/// source. Every `perf-validate*` board case must therefore provision its
/// validator through the standard `c/CMakeLists.txt` session-asset flow: built
/// from the committed C source and uploaded on every run, then downloaded and
/// executed via `${sessionFile:...}` — never a manually pre-staged persistent
/// path. This test fails on the pre-fix configuration.
#[test]
fn perf_validate_board_cases_provision_validator_via_session_assets() {
    let Some(board_dir) = repo_board_case_dir("board-orangepi-5-plus") else {
        return; // out-of-tree checkout: nothing to lint.
    };

    let mut checked = 0usize;
    for entry in fs::read_dir(&board_dir).unwrap() {
        let case_dir = entry.unwrap().path();
        let name = case_dir.file_name().unwrap().to_string_lossy().into_owned();
        if !case_dir.is_dir() || !name.starts_with("perf-validate") {
            continue;
        }
        checked += 1;

        // 1. The validator is built per run from committed C source.
        let cmake = case_dir.join("c").join("CMakeLists.txt");
        assert!(
            cmake.is_file(),
            "board case `{name}` must build its validator through the session-asset \
             `c/CMakeLists.txt` flow; none found at {}",
            cmake.display()
        );

        let toml_path = case_dir.join("board-orangepi-5-plus.toml");
        let body = fs::read_to_string(&toml_path).unwrap();

        // 2. The run command downloads the freshly uploaded session asset...
        assert!(
            body.contains("${sessionFile:"),
            "board case `{name}` must execute the uploaded `${{sessionFile:...}}` validator \
             instead of a pre-staged path ({})",
            toml_path.display()
        );
        // 3. ...and never depends on a manually pre-staged persistent binary.
        assert!(
            !body.contains("/usr/local/bin/perf-validate"),
            "board case `{name}` still references the manually pre-staged \
             /usr/local/bin/perf-validate ({})",
            toml_path.display()
        );
    }

    assert!(
        checked >= 2,
        "expected at least the smp1 anchor and smp8 gate `perf-validate*` cases under {}, found \
         {checked}",
        board_dir.display()
    );
}

/// Resolves `test-suit/starryos/<board>` in the checked-out workspace, or `None`
/// when the crate is built outside the repository tree.
fn repo_board_case_dir(board: &str) -> Option<PathBuf> {
    let board_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("axbuild crate lives at <workspace>/scripts/axbuild")
        .join("test-suit/starryos")
        .join(board);
    board_dir.is_dir().then_some(board_dir)
}

#[test]
fn discovers_board_test_group_and_build_mapping() {
    let root = tempdir().unwrap();
    let build_config = write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    let board_test_config =
        write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus");
    assert_eq!(groups[0].arch, "aarch64");
    assert_eq!(groups[0].target, "aarch64-unknown-none-softfloat");
    assert_eq!(groups[0].build_config_path, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn discovers_board_case_when_case_dir_contains_build_config() {
    let root = tempdir().unwrap();
    let case_dir = root.path().join("test-suit/starryos/smoke");
    fs::create_dir_all(&case_dir).unwrap();
    let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
    fs::write(
        &build_config,
        "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = [\"qemu\"]\nlog = \
         \"Info\"\n",
    )
    .unwrap();
    let board_test_config = case_dir.join("board-orangepi-5-plus.toml");
    fs::write(
        &board_test_config,
        "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \
         \"orangepi@orangepi5plus:~\"\nshell_init_cmd = \"pwd && echo 'test \
         pass'\"\nsuccess_regex = [\"(?m)^test pass\\\\s*$\"]\nfail_regex = []\ntimeout = 300\n",
    )
    .unwrap();

    let groups = discover_board_test_groups(root.path(), None, None).unwrap();

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].name, "smoke");
    assert_eq!(groups[0].board_name, "orangepi-5-plus");
    assert_eq!(groups[0].build_config_path, build_config);
    assert_eq!(groups[0].board_test_config_path, board_test_config);
}

#[test]
fn filters_board_test_group_by_case() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
    write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

    let groups = discover_board_test_groups(root.path(), Some("smoke"), None).unwrap();

    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus", "smoke/vision-five2"]
    );
}

#[test]
fn filters_board_test_groups_by_board() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_starry_board_build_config(root.path(), "vision-five2", "riscv64gc-unknown-none-elf");
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");
    write_board_test_config(root.path(), "orangepi-5-plus", "syscall", "orangepi-5-plus");
    write_board_test_config(root.path(), "vision-five2", "smoke", "vision-five2");

    let groups = discover_board_test_groups(root.path(), None, Some("orangepi-5-plus")).unwrap();

    assert_eq!(
        groups
            .iter()
            .map(|group| format!("{}/{}", group.name, group.board_name))
            .collect::<Vec<_>>(),
        vec!["smoke/orangepi-5-plus", "syscall/orangepi-5-plus"]
    );
}

#[test]
fn rejects_unknown_board_test_board() {
    let root = tempdir().unwrap();
    write_starry_board_build_config(
        root.path(),
        "orangepi-5-plus",
        "aarch64-unknown-none-softfloat",
    );
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let err = discover_board_test_groups(root.path(), None, Some("unknown")).unwrap_err();

    assert!(
        err.to_string()
            .contains("unsupported Starry board test board `unknown`")
    );
    assert!(err.to_string().contains("orangepi-5-plus"));
}

#[test]
fn rejects_missing_mapped_board_build_config() {
    let root = tempdir().unwrap();
    write_board_test_config(root.path(), "orangepi-5-plus", "smoke", "orangepi-5-plus");

    let err = discover_board_test_groups(root.path(), None, None)
        .unwrap_err()
        .to_string();

    assert!(err.contains("not under a build wrapper"));
    assert!(err.contains("smoke"));
}
