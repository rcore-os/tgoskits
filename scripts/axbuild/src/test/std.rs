use std::{
    collections::{BTreeSet, HashSet},
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
};

use anyhow::{Context, bail};
use cargo_metadata::{Metadata, Package};
use clap::Args;

use crate::support::{git::IncrementalPackageSelection, process::run_cargo_status};

const STD_CRATES_CSV: &str = "scripts/test/std_crates.csv";
const PCI_FDT_IRQ_CAPABILITY_TEST: &str =
    "pci_fdt_interrupt_map_requires_and_accepts_registered_intc";

#[derive(Args, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StdTestArgs {
    /// Run std tests only for workspace packages affected since the git ref
    #[arg(long, value_name = "REF")]
    pub(crate) since: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct PackageFeatureProfile {
    name: &'static str,
    no_default_features: bool,
    features: &'static [&'static str],
    name_filter: Option<&'static str>,
    expected_tests: &'static [&'static str],
}

const AX_TASK_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test-api",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("api::"),
        expected_tests: &[
            "api::std_tests::axtask_api_constants_hold",
            "api::std_tests::axtask_api_scheduler_name_hold",
            "api::std_tests::axtask_api_task_registry_functions_exist_hold",
            "api::std_tests::axtask_api_type_aliases_hold",
            "api::tests::task_initialization_precedes_scheduling",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-timer-model",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("future::time::timer_regression_tests::"),
        expected_tests: &[
            "future::time::timer_regression_tests::due_future_work_is_not_republished_as_a_clockevent_deadline",
            "future::time::timer_regression_tests::future_deadline_is_republished_after_the_due_pass_finishes",
            "future::time::timer_regression_tests::future_timer_drop_cancels_the_registration_cpu_after_migration",
            "future::time::timer_regression_tests::future_timer_poll_uses_the_registration_cpu_after_migration",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-lock-contract",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("sync::mutex::tests::"),
        expected_tests: &[
            "sync::mutex::tests::leaked_guard_can_be_released_by_owner_wrapper",
            "sync::mutex::tests::lock_rejects_preemption_disabled_context",
            "sync::mutex::tests::lock_reports_the_external_call_site_to_the_runtime",
            "sync::mutex::tests::try_lock_is_nonblocking",
            "sync::mutex::tests::wrong_owner_force_unlock_is_rejected",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-remote-reschedule",
        no_default_features: false,
        features: &["host-test", "smp", "ipi"],
        name_filter: Some("run_queue::tests::"),
        expected_tests: &[
            "run_queue::tests::forced_remote_reschedule_bypasses_stale_pending",
            "run_queue::tests::remote_reschedule_request_is_coalesced",
            "run_queue::tests::remote_reschedule_send_failure_is_reported_and_keeps_pending",
        ],
    },
];

const AX_HAL_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    no_default_features: false,
    features: &["host-test"],
    name_filter: None,
    expected_tests: &[
        "boot::tests::boot_entropy_is_unavailable_without_firmware",
        "boot::tests::bootargs_facade_is_available",
        "cache::tests::all_cpu_tlb_shootdown_propagates_remote_failure",
        "cache::tests::all_cpu_tlb_shootdown_skips_offline_cpus_then_flushes_local",
        "cache::tests::large_tlb_ranges_switch_to_one_full_invalidation",
        "cache::tests::local_mmu_cache_update_aligns_the_fault_address_once",
        "cache::tests::targeted_tlb_shootdown_skips_unselected_remote_and_local_cpus",
        "irq::tests::irq_entry_preserves_disabled_caller_state",
        "irq::tests::irq_entry_preserves_enabled_caller_state",
        "topology::tests::dummy_topology_only_maps_the_boot_cpu",
    ],
}];

const AX_DRIVER_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test+rtc+starfive-jh7110-dwmmc",
        no_default_features: false,
        features: &["host-test", "rtc", "starfive-jh7110-dwmmc"],
        name_filter: None,
        expected_tests: &[],
    },
    PackageFeatureProfile {
        name: "pci-fdt-irq-capability",
        no_default_features: false,
        features: &["pci"],
        name_filter: Some(PCI_FDT_IRQ_CAPABILITY_TEST),
        expected_tests: &[PCI_FDT_IRQ_CAPABILITY_TEST],
    },
    // The rk3588-cpufreq feature gates the governor busy-attribution tests
    // (the non-monotonic pin regression and its siblings), which are otherwise
    // never compiled by the host-test profile above. This profile lists and
    // runs exactly the `attribution` submodule so CI proves the regression is
    // discovered and executed, not just compiled into a binary that is never
    // asked to run it.
    PackageFeatureProfile {
        name: "host-test+rk3588-cpufreq",
        no_default_features: false,
        features: &["host-test", "rk3588-cpufreq"],
        name_filter: Some("attribution::"),
        expected_tests: &[
            "soc::rockchip::cpufreq::tests::attribution::identity_order_books_each_cpu_under_its_own_cluster",
            "soc::rockchip::cpufreq::tests::attribution::non_monotonic_pin_books_busy_under_the_cluster_it_runs_on",
            "soc::rockchip::cpufreq::tests::attribution::offline_hardware_id_books_nowhere",
            "soc::rockchip::cpufreq::tests::attribution::out_of_range_logical_index_is_refused",
            "soc::rockchip::cpufreq::tests::attribution::single_vcpu_pin_books_under_its_pinned_cluster",
        ],
    },
];

const HOST_TEST_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "host-test",
    no_default_features: false,
    features: &["host-test"],
    name_filter: None,
    expected_tests: &[],
}];

const ALLOC_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "alloc",
    no_default_features: false,
    features: &["alloc"],
    name_filter: None,
    expected_tests: &[],
}];

const FS_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "fs",
    no_default_features: false,
    features: &["fs"],
    name_filter: None,
    expected_tests: &[],
}];

const AX_FS_NG_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "host-test+vfs",
        no_default_features: false,
        features: &["host-test", "vfs"],
        name_filter: None,
        expected_tests: &[],
    },
    PackageFeatureProfile {
        name: "host-test+vfs-reclaim-discovery",
        no_default_features: false,
        features: &["host-test", "vfs"],
        name_filter: Some("reclaim_releases_registry_spin_lock_before_sleepable_file_locks"),
        expected_tests: &[
            "file::cache::reclaim::tests::reclaim_releases_registry_spin_lock_before_sleepable_file_locks",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-resource-rollback-discovery",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("until_controller_shutdown"),
        expected_tests: &[
            "block::runtime::lifecycle::tests::resource_rollback::duplicate_queue_update_keeps_current_and_trailing_queues_until_controller_shutdown",
            "block::runtime::lifecycle::tests::resource_rollback::failed_hctx_start_keeps_current_and_trailing_queues_until_controller_shutdown",
            "block::runtime::lifecycle::tests::resource_rollback::rejected_device_info_update_keeps_emitted_queue_until_controller_shutdown",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-ready-publication-discovery",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("ready_device_rejects_changed_device_info_without_overwriting_epoch"),
        expected_tests: &[
            "block::runtime::lifecycle::tests::publication::ready_device_rejects_changed_device_info_without_overwriting_epoch",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-ready-prefix-discovery",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("provisional_hctx_is_promoted_only_by_a_ready_update"),
        expected_tests: &[
            "block::runtime::lifecycle::tests::publication::provisional_hctx_is_promoted_only_by_a_ready_update",
        ],
    },
    PackageFeatureProfile {
        name: "host-test-lifecycle-teardown-discovery",
        no_default_features: false,
        features: &["host-test"],
        name_filter: Some("block::runtime::lifecycle::tests::teardown::"),
        expected_tests: &[
            "block::runtime::lifecycle::tests::teardown::active_queue_shutdown_failure_is_reported_and_quarantined",
            "block::runtime::lifecycle::tests::teardown::bootstrap_preserves_waiting_for_irq_controller_without_io_queue",
            "block::runtime::lifecycle::tests::teardown::closed_submission_channel_is_retryable_only_while_device_is_ready",
            "block::runtime::lifecycle::tests::teardown::controller_can_register_control_irq_before_creating_an_io_queue",
            "block::runtime::lifecycle::tests::teardown::controller_group_enables_shared_irq_before_unmasking_sources_and_tears_down_once",
            "block::runtime::lifecycle::tests::teardown::detached_queue_shutdown_failure_is_reported_and_quarantined",
            "block::runtime::lifecycle::tests::teardown::failed_terminal_teardown_quarantines_group_controller",
            "block::runtime::lifecycle::tests::teardown::failed_terminal_teardown_quarantines_standalone_irq_registration",
            "block::runtime::lifecycle::tests::teardown::failed_irq_registration_stops_controller_before_dropping_emitted_queue",
            "block::runtime::lifecycle::tests::teardown::group_member_terminal_is_escalated_to_shared_irq_owner",
            "block::runtime::lifecycle::tests::teardown::group_member_watchdog_terminal_is_escalated_to_shared_irq_owner",
            "block::runtime::lifecycle::tests::teardown::group_irq_failure_does_not_bypass_shared_owner",
            "block::runtime::lifecycle::tests::teardown::group_queue_shutdown_failure_is_reported_and_quarantined",
            "block::runtime::lifecycle::tests::teardown::group_teardown_wakes_every_concurrent_waiter",
            "block::runtime::lifecycle::tests::teardown::irq_synchronize_failure_blocks_hardware_shutdown",
            "block::runtime::lifecycle::tests::teardown::last_device_handle_drop_owns_teardown_despite_internal_references",
            "block::runtime::lifecycle::tests::teardown::rejected_shutdown_update_keeps_emitted_queue_quarantined",
            "block::runtime::lifecycle::tests::teardown::runtime_teardown_continues_after_terminal_device_error",
            "block::runtime::lifecycle::tests::teardown::late_hctx_failure_cannot_resurrect_a_stopped_device",
            "block::runtime::lifecycle::tests::teardown::member_shutdown_failure_quarantines_unstopped_group_controller",
            "block::runtime::lifecycle::tests::teardown::partial_group_irq_enable_with_failed_synchronize_quarantines_all_owners",
            "block::runtime::lifecycle::tests::teardown::provisional_group_terminal_waits_for_shared_irq_owner",
            "block::runtime::lifecycle::tests::teardown::teardown_accepts_repeated_device_info_and_releases_resources_in_order",
            "block::runtime::lifecycle::tests::teardown::teardown_releases_queue_when_quiesce_confirms_prior_watchdog_shutdown",
            "block::runtime::lifecycle::tests::teardown::teardown_shutdowns_queue_rolled_back_while_shutdown_is_queued",
        ],
    },
];

const NVME_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "default",
        no_default_features: false,
        features: &[],
        name_filter: None,
        expected_tests: &[],
    },
    PackageFeatureProfile {
        name: "rearm-state-discovery",
        no_default_features: false,
        features: &[],
        name_filter: Some("rearm_during_initialization_preserves_waiting_for_irq_state"),
        expected_tests: &[
            "block::tests::rearm_during_initialization_preserves_waiting_for_irq_state",
        ],
    },
];

const SDMMC_RDIF_FEATURE_PROFILES: &[PackageFeatureProfile] = &[
    PackageFeatureProfile {
        name: "rdif",
        no_default_features: true,
        features: &["rdif"],
        name_filter: None,
        expected_tests: &[],
    },
    PackageFeatureProfile {
        name: "rdif-lifecycle-discovery",
        no_default_features: true,
        features: &["rdif"],
        name_filter: Some("ready_online_smp_repeats_info_without_reissuing_resources"),
        expected_tests: &[
            "sdio::tests::rdif_lifecycle::ready_online_smp_repeats_info_without_reissuing_resources",
        ],
    },
];

const AXBUILD_FEATURE_PROFILES: &[PackageFeatureProfile] = &[PackageFeatureProfile {
    name: "default",
    no_default_features: false,
    features: &[],
    name_filter: None,
    expected_tests: &[],
}];
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CargoTestAction {
    List,
    Run,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CargoTestInvocation {
    package: String,
    no_default_features: bool,
    features: Vec<String>,
    name_filter: Option<String>,
    action: CargoTestAction,
}

impl CargoTestInvocation {
    fn default_for(package: &str) -> Self {
        Self {
            package: package.to_owned(),
            no_default_features: false,
            features: Vec::new(),
            name_filter: None,
            action: CargoTestAction::Run,
        }
    }

    fn for_profile(
        package: &str,
        profile: &PackageFeatureProfile,
        action: CargoTestAction,
    ) -> Self {
        Self {
            package: package.to_owned(),
            no_default_features: profile.no_default_features,
            features: profile
                .features
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
            name_filter: profile.name_filter.map(str::to_owned),
            action,
        }
    }

    fn args(&self) -> Vec<String> {
        let mut args = vec!["test".into(), "-p".into(), self.package.clone()];
        if self.no_default_features {
            args.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            args.push("--features".into());
            args.push(self.features.join(","));
        }
        if let Some(name_filter) = &self.name_filter {
            args.push(name_filter.clone());
        }
        if self.action == CargoTestAction::List {
            args.extend(["--".into(), "--list".into()]);
        }
        args
    }
}

#[derive(Clone, Debug)]
struct CargoRunOutput {
    success: bool,
    stdout: String,
}

pub(crate) fn run_std_test_command(args: &StdTestArgs) -> anyhow::Result<()> {
    let workspace_manifest = crate::context::workspace_manifest_path()?;
    let metadata = if args.since.is_some() {
        crate::context::workspace_metadata_root_manifest_with_deps(&workspace_manifest)
    } else {
        crate::context::workspace_metadata_root_manifest(&workspace_manifest)
    }
    .context("failed to load cargo metadata")?;
    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let known_packages = workspace_package_names(&metadata);
    let csv_path = workspace_root.join(STD_CRATES_CSV);
    let all_packages = load_std_crates(&csv_path, &known_packages)?;
    let packages = match args.since.as_deref() {
        None => all_packages,
        Some(since) => {
            let workspace_packages = workspace_packages(&metadata);
            let selection = crate::support::git::select_incremental_packages(
                &workspace_root,
                &metadata,
                &workspace_packages,
                since,
            )
            .unwrap_or_else(|error| IncrementalPackageSelection::Full {
                reason: format!("incremental std test selection failed: {error:#}"),
            });
            match &selection {
                IncrementalPackageSelection::Packages { changed, affected } => println!(
                    "incremental std tests since {since}: changed [{}], affected [{}]",
                    changed.join(", "),
                    affected.join(", ")
                ),
                IncrementalPackageSelection::Full { reason } => println!(
                    "incremental std test selection fell back to the full whitelist: {reason}"
                ),
            }
            select_std_packages(all_packages, &selection)
        }
    };

    println!(
        "running std tests for {} package(s) from {}",
        packages.len(),
        csv_path.display()
    );
    if packages.is_empty() {
        println!("no affected std test packages selected");
        return Ok(());
    }

    let mut runner = ProcessCargoRunner;
    let failed = run_std_tests(&mut runner, &workspace_root, &packages)?;

    if failed.is_empty() {
        println!("all std tests passed");
        return Ok(());
    }

    eprintln!(
        "std tests failed for {} package(s): {}",
        failed.len(),
        failed.join(", ")
    );
    bail!("std test run failed")
}

fn workspace_packages(metadata: &Metadata) -> Vec<Package> {
    metadata
        .packages
        .iter()
        .filter(|package| metadata.workspace_members.contains(&package.id))
        .cloned()
        .collect()
}

fn select_std_packages(
    mut packages: Vec<String>,
    selection: &IncrementalPackageSelection,
) -> Vec<String> {
    let IncrementalPackageSelection::Packages { affected, .. } = selection else {
        return packages;
    };
    let affected = affected.iter().map(String::as_str).collect::<HashSet<_>>();
    packages.retain(|package| affected.contains(package.as_str()));
    packages
}

fn workspace_package_names(metadata: &Metadata) -> HashSet<String> {
    metadata
        .packages
        .iter()
        .filter(|pkg| metadata.workspace_members.contains(&pkg.id))
        .map(|pkg| pkg.name.to_string())
        .collect()
}

fn load_std_crates(
    csv_path: &Path,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let contents = fs::read_to_string(csv_path)
        .with_context(|| format!("failed to read {}", csv_path.display()))?;
    parse_std_crates_csv(&contents, known_packages)
}

fn parse_std_crates_csv(
    contents: &str,
    known_packages: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    let mut lines = contents.lines().enumerate().filter_map(|(idx, raw)| {
        let line = raw.trim();
        (!line.is_empty()).then_some((idx + 1, line))
    });

    let Some((header_line, header)) = lines.next() else {
        bail!("std crate csv is empty")
    };
    let header = header.trim_start_matches('\u{feff}');
    if header != "package" {
        bail!(
            "invalid header at line {}: expected `package`, found `{}`",
            header_line,
            header
        );
    }

    let mut packages = Vec::new();
    let mut seen = HashSet::new();
    for (line_no, package) in lines {
        if !known_packages.contains(package) {
            bail!(
                "unknown workspace package `{}` at line {}",
                package,
                line_no
            );
        }
        if !seen.insert(package.to_owned()) {
            bail!("duplicate package `{}` at line {}", package, line_no);
        }
        packages.push(package.to_owned());
    }

    Ok(packages)
}

fn run_std_tests<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    packages: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut failed = Vec::new();

    for (index, package) in packages.iter().enumerate() {
        let passed = if let Some(profiles) = package_feature_profiles(package) {
            println!(
                "[{}/{}] running {} std test profile(s) for {}",
                index + 1,
                packages.len(),
                profiles.len(),
                package
            );
            run_feature_profiles(runner, workspace_root, package, profiles)?
        } else {
            let invocation = CargoTestInvocation::default_for(package);
            println!(
                "[{}/{}] cargo {}",
                index + 1,
                packages.len(),
                invocation.args().join(" ")
            );
            runner.run(workspace_root, &invocation)?.success
        };

        if passed {
            println!("ok: {}", package);
        } else {
            eprintln!("failed: {}", package);
            failed.push(package.clone());
        }
    }

    Ok(failed)
}

fn package_feature_profiles(package: &str) -> Option<&'static [PackageFeatureProfile]> {
    match package {
        "arm_vgic"
        | "axdevice"
        | "axfs-ng-vfs"
        | "rsext4"
        | "scope-local"
        | "ax-sync"
        | "axvm"
        | "ax-display"
        | "ax-input"
        | "ax-ipi"
        | "ax-log"
        | "ax-runtime"
        | "ax-api"
        | "rdrive"
        | "axpoll"
        | "ax-net"
        | "dma-api"
        | "buddy-slab-allocator" => Some(HOST_TEST_FEATURE_PROFILES),
        "ax-fs-ng" => Some(AX_FS_NG_FEATURE_PROFILES),
        "ax-io" | "axbacktrace" => Some(ALLOC_FEATURE_PROFILES),
        "ax-hal" => Some(AX_HAL_FEATURE_PROFILES),
        "ax-task" => Some(AX_TASK_FEATURE_PROFILES),
        "ax-driver" => Some(AX_DRIVER_FEATURE_PROFILES),
        "nvme-driver" => Some(NVME_FEATURE_PROFILES),
        "sdmmc-protocol" => Some(SDMMC_RDIF_FEATURE_PROFILES),
        "axbuild" => Some(AXBUILD_FEATURE_PROFILES),
        "axvisor" => Some(FS_FEATURE_PROFILES),
        _ => None,
    }
}

fn run_feature_profiles<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    package: &str,
    profiles: &[PackageFeatureProfile],
) -> anyhow::Result<bool> {
    let mut passed = true;

    for profile in profiles {
        if !run_feature_profile(runner, workspace_root, package, profile)? {
            passed = false;
        }
    }

    Ok(passed)
}

fn run_feature_profile<R: CargoRunner>(
    runner: &mut R,
    workspace_root: &Path,
    package: &str,
    profile: &PackageFeatureProfile,
) -> anyhow::Result<bool> {
    if !profile.expected_tests.is_empty() {
        let list_invocation =
            CargoTestInvocation::for_profile(package, profile, CargoTestAction::List);
        println!("cargo {}", list_invocation.args().join(" "));
        let listed = runner.run(workspace_root, &list_invocation)?;
        if !listed.success {
            eprintln!(
                "profile `{}` failed while listing filtered tests",
                profile.name
            );
            return Ok(false);
        }
        if let Err(err) = validate_discovered_tests(profile, &listed.stdout) {
            eprintln!("profile `{}` test discovery failed: {err:#}", profile.name);
            return Ok(false);
        }
    }

    let run_invocation = CargoTestInvocation::for_profile(package, profile, CargoTestAction::Run);
    println!("cargo {}", run_invocation.args().join(" "));
    let executed = runner.run(workspace_root, &run_invocation)?;
    if !executed.success {
        eprintln!("profile `{}` filtered tests failed", profile.name);
    }
    Ok(executed.success)
}

fn validate_discovered_tests(
    profile: &PackageFeatureProfile,
    listed_stdout: &str,
) -> anyhow::Result<()> {
    let discovered = parse_listed_tests(listed_stdout);
    let expected = profile
        .expected_tests
        .iter()
        .map(|test| (*test).to_owned())
        .collect::<BTreeSet<_>>();

    if discovered.is_empty() {
        bail!(
            "expected [{}], but the filtered command discovered 0 tests",
            expected.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let missing = expected
        .difference(&discovered)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "required tests [{}] were not discovered; discovered [{}]",
            missing.join(", "),
            discovered.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    if profile.name_filter.is_some() && discovered != expected {
        bail!(
            "expected [{}], discovered [{}]",
            expected.iter().cloned().collect::<Vec<_>>().join(", "),
            discovered.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    Ok(())
}

fn parse_listed_tests(listed_stdout: &str) -> BTreeSet<String> {
    listed_stdout
        .lines()
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(str::to_owned)
        .collect()
}

trait CargoRunner {
    fn run(
        &mut self,
        workspace_root: &Path,
        invocation: &CargoTestInvocation,
    ) -> anyhow::Result<CargoRunOutput>;
}

struct ProcessCargoRunner;

impl CargoRunner for ProcessCargoRunner {
    fn run(
        &mut self,
        workspace_root: &Path,
        invocation: &CargoTestInvocation,
    ) -> anyhow::Result<CargoRunOutput> {
        let args = invocation.args();
        if invocation.action == CargoTestAction::Run {
            return Ok(CargoRunOutput {
                success: run_cargo_status(workspace_root, &args)?,
                stdout: String::new(),
            });
        }

        let output = Command::new("cargo")
            .current_dir(workspace_root)
            .args(&args)
            .output()
            .with_context(|| format!("failed to spawn `cargo {}`", args.join(" ")))?;
        io::stdout()
            .write_all(&output.stdout)
            .context("failed to print cargo stdout")?;
        io::stderr()
            .write_all(&output.stderr)
            .context("failed to print cargo stderr")?;

        Ok(CargoRunOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, path::PathBuf};

    use super::*;

    fn known_packages() -> HashSet<String> {
        HashSet::from(["ax-api".to_string(), "ax-hal".to_string()])
    }

    struct FakeCargoRunner {
        results: HashMap<CargoTestInvocation, CargoRunOutput>,
        invocations: Vec<(PathBuf, CargoTestInvocation)>,
    }

    impl FakeCargoRunner {
        fn succeeding() -> Self {
            Self {
                results: HashMap::new(),
                invocations: Vec::new(),
            }
        }

        fn with_status(mut self, invocation: CargoTestInvocation, success: bool) -> Self {
            self.results.insert(
                invocation,
                CargoRunOutput {
                    success,
                    stdout: String::new(),
                },
            );
            self
        }

        fn with_listing(
            mut self,
            package: &str,
            profile: &PackageFeatureProfile,
            tests: &[&str],
        ) -> Self {
            self.results.insert(
                CargoTestInvocation::for_profile(package, profile, CargoTestAction::List),
                CargoRunOutput {
                    success: true,
                    stdout: render_test_list(tests),
                },
            );
            self
        }

        fn with_ax_task_discovery(mut self) -> Self {
            for profile in AX_TASK_FEATURE_PROFILES {
                self = self.with_listing("ax-task", profile, profile.expected_tests);
            }
            self
        }

        fn with_profile_discovery(
            mut self,
            package: &str,
            profiles: &[PackageFeatureProfile],
        ) -> Self {
            for profile in profiles {
                if !profile.expected_tests.is_empty() {
                    self = self.with_listing(package, profile, profile.expected_tests);
                }
            }
            self
        }

        fn with_ax_hal_discovery(self) -> Self {
            let profile = &AX_HAL_FEATURE_PROFILES[0];
            self.with_listing("ax-hal", profile, profile.expected_tests)
        }
    }

    impl CargoRunner for FakeCargoRunner {
        fn run(
            &mut self,
            workspace_root: &Path,
            invocation: &CargoTestInvocation,
        ) -> anyhow::Result<CargoRunOutput> {
            self.invocations
                .push((workspace_root.to_path_buf(), invocation.clone()));
            Ok(self
                .results
                .get(invocation)
                .cloned()
                .unwrap_or(CargoRunOutput {
                    success: true,
                    stdout: String::new(),
                }))
        }
    }

    fn render_test_list(tests: &[&str]) -> String {
        let mut output = tests
            .iter()
            .map(|test| format!("{test}: test"))
            .collect::<Vec<_>>()
            .join("\n");
        output.push_str(&format!("\n\n{} tests, 0 benchmarks\n", tests.len()));
        output
    }

    #[test]
    fn parses_valid_std_csv() {
        let packages =
            parse_std_crates_csv("package\nax-api\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-api".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn incremental_selection_keeps_affected_whitelist_order() {
        let packages = ["ax-api", "ax-hal", "ax-task"].map(str::to_string).to_vec();
        let selection = IncrementalPackageSelection::Packages {
            changed: vec!["ax-task".to_string()],
            affected: vec!["ax-task".to_string(), "ax-api".to_string()],
        };

        let selected = select_std_packages(packages, &selection);

        assert_eq!(selected, vec!["ax-api".to_string(), "ax-task".to_string()]);
    }

    #[test]
    fn incremental_selection_accepts_no_affected_std_packages() {
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let selection = IncrementalPackageSelection::Packages {
            changed: vec!["standalone".to_string()],
            affected: vec!["standalone".to_string()],
        };

        let selected = select_std_packages(packages, &selection);

        assert!(selected.is_empty());
    }

    #[test]
    fn incremental_full_fallback_keeps_every_std_package() {
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let selection = IncrementalPackageSelection::Full {
            reason: "fixture".to_string(),
        };

        let selected = select_std_packages(packages.clone(), &selection);

        assert_eq!(selected, packages);
    }

    #[test]
    fn parses_std_csv_with_blank_lines() {
        let packages =
            parse_std_crates_csv("\npackage\n\nax-api\n\nax-hal\n", &known_packages()).unwrap();

        assert_eq!(packages, vec!["ax-api".to_string(), "ax-hal".to_string()]);
    }

    #[test]
    fn rejects_empty_std_csv() {
        let err = parse_std_crates_csv("", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("std crate csv is empty"));
    }

    #[test]
    fn rejects_invalid_header() {
        let err = parse_std_crates_csv("crate\nax-api\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("invalid header"));
    }

    #[test]
    fn rejects_unknown_package() {
        let err = parse_std_crates_csv("package\nunknown\n", &known_packages()).unwrap_err();

        assert!(
            err.to_string()
                .contains("unknown workspace package `unknown`")
        );
    }

    #[test]
    fn rejects_duplicate_package() {
        let err = parse_std_crates_csv("package\nax-api\nax-api\n", &known_packages()).unwrap_err();

        assert!(err.to_string().contains("duplicate package `ax-api`"));
    }

    #[test]
    fn workspace_package_name_extraction_reads_current_workspace() {
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .exec()
            .unwrap();
        let names = workspace_package_names(&metadata);

        assert!(names.contains("axbuild"));
        assert!(names.contains("tg-xtask"));
    }

    #[test]
    fn std_test_runner_collects_all_failures() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let ax_hal_profile = &AX_HAL_FEATURE_PROFILES[0];
        let mut runner = FakeCargoRunner::succeeding()
            .with_ax_hal_discovery()
            .with_status(
                CargoTestInvocation::for_profile("ax-hal", ax_hal_profile, CargoTestAction::Run),
                false,
            );

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-hal".to_string()]);
    }

    #[test]
    fn std_test_runner_returns_empty_failures_when_all_pass() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-api".to_string(), "ax-hal".to_string()];
        let mut runner = FakeCargoRunner::succeeding().with_ax_hal_discovery();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
    }

    #[test]
    fn ax_hal_uses_the_host_profile_and_discovers_required_tests() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-hal".to_string()];
        let mut runner = FakeCargoRunner::succeeding().with_ax_hal_discovery();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        assert!(runner.invocations.iter().any(|(_, invocation)| {
            invocation.args()
                == vec![
                    "test",
                    "-p",
                    "ax-hal",
                    "--features",
                    "host-test",
                    "--",
                    "--list",
                ]
        }));
        assert!(runner.invocations.iter().any(|(_, invocation)| {
            invocation.args() == vec!["test", "-p", "ax-hal", "--features", "host-test"]
        }));
    }

    #[test]
    fn ax_driver_rejects_missing_pci_fdt_irq_capability_test() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-driver".to_string()];
        let mut runner = FakeCargoRunner::succeeding();

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-driver"]);
        let pci_profile = &AX_DRIVER_FEATURE_PROFILES[1];
        assert!(!runner.invocations.iter().any(|(_, invocation)| {
            invocation
                == &CargoTestInvocation::for_profile("ax-driver", pci_profile, CargoTestAction::Run)
        }));
    }

    #[test]
    fn lifecycle_packages_select_full_and_discovery_profiles() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = [
            "ax-fs-ng",
            "ahci-driver",
            "nvme-driver",
            "rdif-block",
            "sdmmc-protocol",
            "axbuild",
        ]
        .map(str::to_string)
        .to_vec();
        let mut runner = FakeCargoRunner::succeeding()
            .with_profile_discovery("ax-fs-ng", AX_FS_NG_FEATURE_PROFILES)
            .with_profile_discovery("nvme-driver", NVME_FEATURE_PROFILES)
            .with_profile_discovery("sdmmc-protocol", SDMMC_RDIF_FEATURE_PROFILES)
            .with_profile_discovery("axbuild", AXBUILD_FEATURE_PROFILES);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert!(failed.is_empty());
        let invocations = runner
            .invocations
            .iter()
            .map(|(_, invocation)| invocation)
            .collect::<Vec<_>>();
        assert!(invocations.contains(&&CargoTestInvocation::for_profile(
            "ax-fs-ng",
            &AX_FS_NG_FEATURE_PROFILES[0],
            CargoTestAction::Run,
        )));
        assert!(invocations.contains(&&CargoTestInvocation::default_for("ahci-driver")));
        assert!(invocations.contains(&&CargoTestInvocation::for_profile(
            "nvme-driver",
            &NVME_FEATURE_PROFILES[0],
            CargoTestAction::Run,
        )));
        assert!(invocations.contains(&&CargoTestInvocation::default_for("rdif-block")));
        assert!(invocations.contains(&&CargoTestInvocation::for_profile(
            "sdmmc-protocol",
            &SDMMC_RDIF_FEATURE_PROFILES[0],
            CargoTestAction::Run,
        )));
        assert!(invocations.contains(&&CargoTestInvocation::for_profile(
            "axbuild",
            &AXBUILD_FEATURE_PROFILES[0],
            CargoTestAction::Run,
        )));
    }

    #[test]
    fn profile_discovery_mismatch_fails_without_running_that_profile() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-task".to_string()];
        let profile = &AX_TASK_FEATURE_PROFILES[0];
        let mut runner = FakeCargoRunner::succeeding()
            .with_ax_task_discovery()
            .with_listing("ax-task", profile, &["api::std_tests::unexpected"]);

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-task"]);
        assert!(!runner.invocations.iter().any(|(_, invocation)| {
            invocation
                == &CargoTestInvocation::for_profile("ax-task", profile, CargoTestAction::Run)
        }));
    }

    #[test]
    fn profile_discovery_rejects_zero_tests() {
        let err = validate_discovered_tests(&AX_TASK_FEATURE_PROFILES[0], "0 tests, 0 benchmarks")
            .unwrap_err();

        assert!(err.to_string().contains("discovered 0 tests"));
    }

    #[test]
    fn unfiltered_profile_discovery_accepts_additional_tests() {
        let profile = &AX_HAL_FEATURE_PROFILES[0];
        let mut tests = profile.expected_tests.to_vec();
        tests.push("timers::tests::an_additional_regression");

        validate_discovered_tests(profile, &render_test_list(&tests)).unwrap();
    }

    #[test]
    fn filtered_profile_discovery_rejects_additional_tests() {
        let profile = &AX_DRIVER_FEATURE_PROFILES[1];
        let tests = [
            PCI_FDT_IRQ_CAPABILITY_TEST,
            "pci_fdt_interrupt_map_unrelated_test",
        ];

        let err = validate_discovered_tests(profile, &render_test_list(&tests)).unwrap_err();

        assert!(err.to_string().contains("expected ["));
    }

    #[test]
    fn cargo_execution_failures_are_aggregated_across_profiles_and_packages() {
        let root = PathBuf::from("/tmp/workspace");
        let packages = vec!["ax-task".to_string(), "ax-api".to_string()];
        let failed_profile = &AX_TASK_FEATURE_PROFILES[0];
        let mut runner = FakeCargoRunner::succeeding()
            .with_ax_task_discovery()
            .with_status(
                CargoTestInvocation::for_profile("ax-task", failed_profile, CargoTestAction::Run),
                false,
            )
            .with_status(
                CargoTestInvocation::for_profile(
                    "ax-api",
                    &HOST_TEST_FEATURE_PROFILES[0],
                    CargoTestAction::Run,
                ),
                false,
            );

        let failed = run_std_tests(&mut runner, &root, &packages).unwrap();

        assert_eq!(failed, vec!["ax-task", "ax-api"]);
    }
}
