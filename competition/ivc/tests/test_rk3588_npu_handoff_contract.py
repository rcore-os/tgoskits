from __future__ import annotations

import re
import unittest
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older
    import tomli as tomllib


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
AX_DRIVER_MANIFEST = REPOSITORY_ROOT / "drivers/ax-driver/Cargo.toml"
AX_DRIVER_LIB = REPOSITORY_ROOT / "drivers/ax-driver/src/lib.rs"
HANDOFF_SOURCE = (
    REPOSITORY_ROOT / "drivers/ax-driver/src/soc/rockchip/npu_handoff.rs"
)
AXVISOR_MANIFEST = REPOSITORY_ROOT / "os/axvisor/Cargo.toml"
AXVISOR_MAIN = REPOSITORY_ROOT / "os/axvisor/src/main.rs"
AXVISOR_HOST_COMMAND = (
    REPOSITORY_ROOT / "os/axvisor/src/shell/command/host.rs"
)
HOST_BUILD_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-rknpu-smoke.toml"
)
GUEST_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-rknpu-smoke.toml"
)
GUEST_DTS = REPOSITORY_ROOT / "competition/ivc/starry/orangepi-5-plus-rknpu.dts"
STARRY_BUILD_CONFIG = (
    REPOSITORY_ROOT / "competition/ivc/config/starry-aarch64-rknpu.toml"
)
STARRY_ROOTFS_BUILDER = (
    REPOSITORY_ROOT / "competition/ivc/starry/build-rknpu-rootfs.sh"
)
STARRY_AUTORUN = (
    REPOSITORY_ROOT / "competition/ivc/starry/rknpu-offline-autorun.sh"
)
STARRY_ARTIFACT_BUILDER = (
    REPOSITORY_ROOT / "competition/ivc/starry/build-rknpu-offline.sh"
)
RKNPU_RUNNER_SOURCE = (
    REPOSITORY_ROOT / "competition/ivc/model/thermal_rknn_linux_reference.cpp"
)
RKNPU_BOARD_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/board-orangepi-5-plus-rknpu-smoke.toml"
)
RKNPU_STAGER = REPOSITORY_ROOT / "competition/ivc/stage-rknpu-offline.sh"
RKNPU_RUNNER = REPOSITORY_ROOT / "competition/ivc/run-rknpu-offline.sh"
BOARD_RUNNER = REPOSITORY_ROOT / "competition/ivc/orangepi/board-runner.sh"
SERVICE_DTB_PREPARER = (
    REPOSITORY_ROOT / "competition/ivc/orangepi/prepare-service-dtb.sh"
)
RKNPU_CONTROL_ROOTFS_BUILDER = (
    REPOSITORY_ROOT / "competition/ivc/starry/build-rknpu-control-rootfs.sh"
)
RKNPU_CONTROL_AUTORUN = REPOSITORY_ROOT / "competition/ivc/starry/autorun.sh"
RKNPU_CONTROL_RUNNER = REPOSITORY_ROOT / "competition/ivc/run-orangepi-5-plus.sh"
RKNPU_CONTROL_STAGER = REPOSITORY_ROOT / "competition/ivc/stage-rknpu-control.sh"
RKNPU_CONTROL_HARVESTER = (
    REPOSITORY_ROOT / "competition/ivc/orangepi/harvest-result.sh"
)
RKNPU_CONTROL_BRIDGE = REPOSITORY_ROOT / "tools/ivcproto/csrc/rknn_bridge.c"
RKNPU_RUST_BACKEND = REPOSITORY_ROOT / "tools/ivcproto/src/rknn.rs"
RKNPU_CONTROL_HOST_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-rknpu-control-smoke.toml"
)
RKNPU_CONTROL_GUEST_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-rknpu-control-smoke.toml"
)

NPU_CORE_RANGES = (
    ("rknpu-core0", 0xFDAB_0000, 0xFDAB_0000, 0x1_0000, 0),
    ("rknpu-core1", 0xFDAC_0000, 0xFDAC_0000, 0x1_0000, 0),
    ("rknpu-core2", 0xFDAD_0000, 0xFDAD_0000, 0x1_0000, 0),
)


class Rk3588NpuHandoffContractTests(unittest.TestCase):
    def test_host_feature_is_distinct_from_the_submit_driver(self) -> None:
        with AX_DRIVER_MANIFEST.open("rb") as source:
            manifest = tomllib.load(source)

        features = manifest["features"]
        self.assertEqual(
            set(features["rk3588-npu-handoff"]),
            {"rockchip-pm", "rockchip-soc", "dep:arm-scmi-rs"},
        )
        self.assertNotIn("rknpu", features["rk3588-npu-handoff"])

        lib_source = AX_DRIVER_LIB.read_text(encoding="utf-8")
        self.assertIn(
            '#[cfg(all(feature = "rknpu", '
            'not(feature = "rk3588-npu-handoff")))]\n'
            "pub mod rknpu;",
            lib_source,
        )

    def test_axvisor_requires_completed_handoff_before_vm_creation(self) -> None:
        with AXVISOR_MANIFEST.open("rb") as source:
            manifest = tomllib.load(source)
        self.assertEqual(
            manifest["features"]["rk3588-npu-handoff"],
            ["ax-driver/rk3588-npu-handoff"],
        )

        main_source = AXVISOR_MAIN.read_text(encoding="utf-8")
        handoff_check = main_source.index("require_rk3588_npu_handoff")
        vm_initialization = main_source.index("manager.init_default_vms()")
        self.assertLess(handoff_check, vm_initialization)

    def test_host_handoff_validates_every_owned_resource(self) -> None:
        source = HANDOFF_SOURCE.read_text(encoding="utf-8")

        for marker in (
            "0xfdab_0000",
            "0xfdac_0000",
            "0xfdad_0000",
            "EXPECTED_POWER_DOMAINS",
            "EXPECTED_CLOCKS",
            "EXPECTED_RESETS",
            "SCMI_NPU_CLOCK_ID",
            "SCMI_NPU_CLOCK_RATE_HZ",
            "AXVISOR_RK3588_NPU_HANDOFF_READY",
            "AXVISOR_RK3588_NPU_RESOURCES",
            "AXVISOR_RK3588_NPU_SCMI",
            "AXVISOR_RK3588_NPU_OWNERSHIP",
            "report_rk3588_npu_handoff",
            "host_submit=false",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, source)

    def test_handoff_evidence_is_redundant_and_paced_by_axvisor(self) -> None:
        main_source = AXVISOR_MAIN.read_text(encoding="utf-8")

        self.assertIn("NPU_HANDOFF_MARKER_COPIES: usize = 5", main_source)
        self.assertIn("NPU_HANDOFF_MARKER_INTERVAL_MS: u64 = 100", main_source)
        self.assertIn("write_rk3588_npu_handoff_markers", main_source)
        self.assertIn("report_rk3588_npu_handoff", main_source)
        self.assertRegex(
            main_source,
            re.compile(
                r"write_rk3588_npu_handoff_markers\(\).*?"
                r"std::thread::sleep\(\s*core::time::Duration::from_millis\(\s*"
                r"NPU_HANDOFF_MARKER_INTERVAL_MS\s*,?\s*\)\s*\)",
                re.DOTALL,
            ),
        )

    def test_smoke_build_enables_handoff_without_host_rknpu(self) -> None:
        with HOST_BUILD_CONFIG.open("rb") as source:
            config = tomllib.load(source)

        self.assertIn("rk3588-npu-handoff", config["features"])
        self.assertNotIn("rknpu", config["features"])
        self.assertEqual(
            config["vm_configs"],
            [
                "competition/ivc/config/"
                "orangepi-5-plus-starry-smp2-rknpu-smoke.toml"
            ],
        )
        self.assertEqual(
            config["success_regex"],
            [r"(?m)^AXVISOR_HOST_FILESYSTEM_SYNCED\r?$"],
        )

    def test_only_npu_core_registers_are_passed_to_the_guest(self) -> None:
        with GUEST_CONFIG.open("rb") as source:
            config = tomllib.load(source)

        self.assertEqual(config["devices"]["interrupt_mode"], "emulated")
        self.assertEqual(
            tuple(tuple(device) for device in config["devices"]["passthrough_devices"]),
            NPU_CORE_RANGES,
        )
        self.assertEqual(config["devices"]["passthrough_addresses"], [])
        self.assertEqual(
            config["kernel"]["dtb_path"],
            "/home/orangepi/axvisor-guest/starry-orangepi-5-plus-rknpu.dtb",
        )

        memory_base, memory_size, _, memory_map_type = config["kernel"][
            "memory_regions"
        ][0]
        self.assertLessEqual(memory_base + memory_size, 1 << 32)
        self.assertEqual(memory_map_type, 2, "NPU DMA memory must be identity reserved")
        for _, base_gpa, base_hpa, length, irq_id in NPU_CORE_RANGES:
            self.assertEqual(base_gpa, base_hpa)
            self.assertEqual(length, 0x1_0000)
            self.assertEqual(irq_id, 0)

    def test_guest_dts_exposes_polling_npu_without_host_resources(self) -> None:
        source = GUEST_DTS.read_text(encoding="utf-8")
        node_match = re.search(
            r"npu@fdab0000\s*\{(?P<body>.*?)\n\s*\};",
            source,
            re.DOTALL,
        )
        self.assertIsNotNone(node_match)
        body = node_match.group("body")
        self.assertIn('compatible = "rockchip,rk3588-rknpu";', body)
        self.assertRegex(
            body,
            re.compile(
                r"reg\s*=\s*<0\s+0xfdab0000\s+0\s+0x10000>,\s*"
                r"<0\s+0xfdac0000\s+0\s+0x10000>,\s*"
                r"<0\s+0xfdad0000\s+0\s+0x10000>;",
                re.DOTALL,
            ),
        )
        self.assertIn("dma-coherent;", body)
        self.assertIn('status = "okay";', body)
        for forbidden in (
            "interrupts",
            "iommus",
            "clocks",
            "resets",
            "power-domains",
            "operating-points-v2",
            "supply",
        ):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, body)

    def test_guest_dts_exposes_the_ivc_virtio_net_transport(self) -> None:
        source = GUEST_DTS.read_text(encoding="utf-8")
        node_match = re.search(
            r"virtio_mmio@a001000\s*\{(?P<body>.*?)\n\s*\};",
            source,
            re.DOTALL,
        )
        self.assertIsNotNone(node_match)
        body = node_match.group("body")
        self.assertIn('compatible = "virtio,mmio";', body)
        self.assertIn("reg = <0 0x0a001000 0 0x1000>;", body)
        self.assertIn("interrupts = <0 24 1>;", body)
        self.assertIn("dma-coherent;", body)

    def test_host_dtb_reserves_the_identity_mapped_dma_carveout(self) -> None:
        preparer = SERVICE_DTB_PREPARER.read_text(encoding="utf-8")
        runner = BOARD_RUNNER.read_text(encoding="utf-8")

        for marker in (
            "axvisor-rknpu-dma@80000000",
            "0x80000000",
            "0x10000000",
            "no-map",
            "BOARD_SERVICE_DTB_RKNPU_DMA_RESERVED",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, preparer)
        self.assertIn("rk3588-npu-handoff", runner)
        self.assertIn("--rknpu-dma-carveout", runner)

    def test_starry_guest_build_has_only_required_device_features(self) -> None:
        with STARRY_BUILD_CONFIG.open("rb") as source:
            config = tomllib.load(source)

        self.assertEqual(
            config["features"],
            ["ax-driver/virtio-blk", "ax-driver/virtio-net", "rknpu"],
        )
        self.assertEqual(config["target"], "aarch64-unknown-none-softfloat")

        builder = STARRY_ARTIFACT_BUILDER.read_text(encoding="utf-8")
        self.assertIn("starry-aarch64-rknpu.toml", builder)
        self.assertIn("--smp 2", builder)
        self.assertIn("orangepi-5-plus-rknpu.dts", builder)
        self.assertIn("build-rknpu-rootfs.sh", builder)

    def test_rknpu_rootfs_carries_audited_dynamic_runtime_and_atomic_results(
        self,
    ) -> None:
        builder = STARRY_ROOTFS_BUILDER.read_text(encoding="utf-8")
        autorun = STARRY_AUTORUN.read_text(encoding="utf-8")

        for dependency in (
            "ld-linux-aarch64.so.1",
            "libc.so.6",
            "libpthread.so.0",
            "libdl.so.2",
            "libm.so.6",
            "libstdc++.so.6",
            "libgcc_s.so.1",
            "librknnrt.so",
        ):
            with self.subTest(dependency=dependency):
                self.assertIn(dependency, builder)
        for marker in (
            "aarch64-linux-gnu-readelf",
            "thermal_rknn_linux_reference.cpp",
            "thermal-4x6x1-v1-rk3588-fp16.rknn",
            "corpus.csv",
            "rknpu-offline-autorun.sh",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, builder)

        self.assertIn("raw.csv.partial", autorun)
        self.assertIn("expected_raw_lines=10001", autorun)
        self.assertIn("THERMAL_RKNN_STARRY_PASS", autorun)
        self.assertIn("THERMAL_RKNN_STARRY_FAIL", autorun)
        self.assertLess(autorun.index("sync"), autorun.index("poweroff -f"))

    def test_starry_runner_evidence_markers_are_redundant_and_paced(self) -> None:
        runner_source = RKNPU_RUNNER_SOURCE.read_text(encoding="utf-8")
        autorun = STARRY_AUTORUN.read_text(encoding="utf-8")

        for option in (
            "--evidence-marker-copies",
            "--evidence-marker-interval-ms",
        ):
            with self.subTest(option=option):
                self.assertIn(option, runner_source)
                self.assertIn(option, autorun)
        self.assertIn("evidence_marker_copies = 1", runner_source)
        self.assertIn("write_redundant_marker", runner_source)
        self.assertIn("runtime_api_compatibility_identity", runner_source)
        self.assertIn("hex_encode(api_compatibility_identity)", runner_source)
        self.assertEqual(runner_source.count("write_redundant_marker(options"), 11)
        self.assertIn("std::this_thread::sleep_for", runner_source)
        self.assertIn("--evidence-marker-copies 5", autorun)
        self.assertIn("--evidence-marker-interval-ms 100", autorun)
        for marker in (
            "IVC_RKNN_RUNTIME_API",
            "IVC_RKNN_RUNTIME_DRIVER",
            "IVC_RKNN_RESULT_META",
            "IVC_RKNN_RESULT_ACCURACY",
            "IVC_RKNN_RESULT_ERROR",
            "IVC_RKNN_RESULT_HEALTH",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, runner_source)
        self.assertIn(
            "THERMAL_RKNN_STARRY_RAW schema=1 vectors=$vectors "
            "sha256=$raw_sha256",
            autorun,
        )
        self.assertNotIn("backend=rknn-npu model_sha256=", autorun)

    def test_starry_resource_gate_repeats_context_lifecycle_and_persists_metrics(
        self,
    ) -> None:
        runner_source = RKNPU_RUNNER_SOURCE.read_text(encoding="utf-8")
        builder = STARRY_ROOTFS_BUILDER.read_text(encoding="utf-8")
        autorun = STARRY_AUTORUN.read_text(encoding="utf-8")
        board_runner = RKNPU_RUNNER.read_text(encoding="utf-8")

        for marker in (
            "--lifecycle-cycles",
            "--resource-output",
            "/proc/self/status",
            "IVC_RKNN_LIFECYCLE",
            "IVC_RKNN_MEMORY_BASELINE",
            "IVC_RKNN_MEMORY_FINAL",
            "cold_init_us",
            "rknn_destroy",
        ):
            with self.subTest(source_marker=marker):
                self.assertIn(marker, runner_source)
        for profile_value in (
            "lifecycle_cycles=20",
            "maximum_post_destroy_growth_kib=4096",
            "maximum_peak_rss_kib=524288",
            "minimum_rootfs_available_percent_x100=2000",
        ):
            with self.subTest(profile_value=profile_value):
                self.assertIn(profile_value, builder)
        for autorun_marker in (
            "resources.txt.partial",
            "resources.txt.sha256",
            '--lifecycle-cycles "$lifecycle_cycles"',
            '--resource-output "$RESOURCE_PARTIAL"',
            "THERMAL_RKNN_STARRY_RESOURCE",
            "THERMAL_RKNN_STARRY_DEVICE",
            "rootfs_available_percent_x100",
        ):
            with self.subTest(autorun_marker=autorun_marker):
                self.assertIn(autorun_marker, autorun)
        for board_marker in (
            "/var/lib/rknn/resources.txt",
            "/var/lib/rknn/resources.txt.sha256",
            "--resource",
            "--resource-manifest",
        ):
            with self.subTest(board_marker=board_marker):
                self.assertIn(board_marker, board_runner)

    def test_rknpu_control_backend_reuses_the_starry_zephyr_full_loop(self) -> None:
        bridge = RKNPU_CONTROL_BRIDGE.read_text(encoding="utf-8")
        rust_backend = RKNPU_RUST_BACKEND.read_text(encoding="utf-8")
        builder = RKNPU_CONTROL_ROOTFS_BUILDER.read_text(encoding="utf-8")
        autorun = RKNPU_CONTROL_AUTORUN.read_text(encoding="utf-8")
        runner = RKNPU_CONTROL_RUNNER.read_text(encoding="utf-8")
        stager = RKNPU_CONTROL_STAGER.read_text(encoding="utf-8")
        harvester = RKNPU_CONTROL_HARVESTER.read_text(encoding="utf-8")
        with RKNPU_CONTROL_HOST_CONFIG.open("rb") as source:
            host_config = tomllib.load(source)
        with RKNPU_CONTROL_GUEST_CONFIG.open("rb") as source:
            guest_config = tomllib.load(source)

        self.assertIn("rk3588-npu-handoff", host_config["features"])
        self.assertEqual(len(host_config["vm_configs"]), 2)
        self.assertTrue(
            host_config["vm_configs"][1].endswith("orangepi-5-plus-zephyr-smoke.toml")
        )
        self.assertEqual(
            guest_config["devices"]["passthrough_devices"],
            [list(core_range) for core_range in NPU_CORE_RANGES],
        )
        self.assertTrue(
            any(
                device[0] == "virtio-net-starry"
                for device in guest_config["devices"]["emu_devices"]
            )
        )
        for marker in (
            "rknn_init",
            "RKNN_QUERY_PERF_RUN",
            "rknn_outputs_release",
            "rknn_destroy",
        ):
            with self.subTest(bridge_marker=marker):
                self.assertIn(marker, bridge)
        for marker in (
            "pub struct RknnController",
            "command_from_output",
            "positive_device_times",
            "device_p99_us",
        ):
            with self.subTest(rust_marker=marker):
                self.assertIn(marker, rust_backend)
        for marker in (
            "--features rknn",
            "aarch64-unknown-linux-gnu",
            "libivc_rknn_bridge.a",
            "librknnrt.so",
            "ivc_backend=rknn-npu",
        ):
            with self.subTest(builder_marker=marker):
                self.assertIn(marker, builder)
        for marker in (
            "IVC-STARRY-RKNN-DEVICE",
            "IVC-STARRY-RKNN-MODEL",
            "IVC-STARRY-RKNN-RAW",
            "--rknn-model",
            "--rknn-evidence",
        ):
            with self.subTest(autorun_marker=marker):
                self.assertIn(marker, autorun)
        for marker in (
            "rknpu-smoke",
            "rknpu-full",
            "IVC-RKNN-RUNTIME",
            "IVC-RKNN-RESULT",
            "ORANGEPI_IVC_RKNN_CSV",
            "rknn.csv.gz",
            "--rknn-csv",
            "--expected-rknn-model-sha256",
            "--expected-rknn-runtime-api",
            "stage-rknpu-control.sh",
        ):
            with self.subTest(runner_marker=marker):
                self.assertIn(marker, runner)
        for marker in (
            "/var/lib/ivc/rknn.csv",
            "/var/lib/ivc/rknn.csv.sha256",
            "BOARD_GUEST_RKNN_MANIFEST",
            "BOARD_RKNN_RESULT_HARVESTED",
        ):
            with self.subTest(harvest_marker=marker):
                self.assertIn(marker, harvester)
        self.assertIn("starry-ivc-rootfs-rknpu-smoke.img", stager)
        self.assertIn("starry-ivc-rootfs-rknpu.img", stager)

    def test_board_flow_requires_guest_pass_and_host_filesystem_sync(self) -> None:
        with RKNPU_BOARD_CONFIG.open("rb") as source:
            board_config = tomllib.load(source)

        stager = RKNPU_STAGER.read_text(encoding="utf-8")
        runner = RKNPU_RUNNER.read_text(encoding="utf-8")
        self.assertEqual(
            board_config["success_regex"],
            [r"(?m)^AXVISOR_HOST_FILESYSTEM_SYNCED\r?$"],
        )
        for marker in (
            "AXVISOR_RK3588_NPU_HANDOFF_READY",
            "IVC_RKNN_PROGRESS completed=10000",
            "THERMAL_RKNN_STARRY_PASS",
            "AXVISOR_SNAPSHOT_SYNC_OK",
            "AXVISOR_HOST_FILESYSTEM_SYNCED",
            "THERMAL_RKNN_STARRY_FAIL",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, runner)
        for artifact in (
            "starryos-rknpu.bin",
            "starry-orangepi-5-plus-rknpu.dtb",
            "starry-rknpu-rootfs-smoke.img",
        ):
            with self.subTest(artifact=artifact):
                self.assertIn(artifact, stager)
        staging_call = 'bash "$rknpu_stager"'
        self.assertIn(
            "rknpu_stager=$script_dir/stage-rknpu-offline.sh",
            runner,
        )
        self.assertIn(staging_call, runner)
        self.assertLess(
            runner.index(staging_call),
            runner.index('start_lease "$result_dir/pre-run-board-connect.log"'),
        )
        self.assertIn("ORANGEPI_AXVISOR_SHUTDOWN_MARKER_REQUIRED=1", runner)
        self.assertIn("board-orangepi-5-plus-rknpu-smoke.toml", runner)
        self.assertIn("axvisor-orangepi-5-plus-rknpu-smoke.toml", runner)

    def test_board_lease_log_exists_before_the_background_reader_starts(self) -> None:
        runner = RKNPU_RUNNER.read_text(encoding="utf-8")
        start_lease = re.search(
            r"start_lease\(\)\s*\{(?P<body>.*?)\n\}",
            runner,
            re.DOTALL,
        )
        self.assertIsNotNone(start_lease)
        body = start_lease.group("body")
        create_log = ': >"$lease_log"'
        launch_connector = 'cargo xtask board connect -b "$board_type"'
        self.assertIn(create_log, body)
        self.assertIn(launch_connector, body)
        self.assertLess(body.index(create_log), body.index(launch_connector))

    def test_board_flow_extracts_and_audits_snapshot_inputs(self) -> None:
        runner = RKNPU_RUNNER.read_text(encoding="utf-8")

        for snapshot_path in (
            "/opt/thermal-rknn/thermal_rknn_reference",
            "/opt/thermal-rknn/thermal-4x6x1-v1-rk3588-fp16.rknn",
            "/opt/thermal-rknn/corpus.csv",
            "/opt/thermal-rknn/lib/librknnrt.so",
        ):
            with self.subTest(snapshot_path=snapshot_path):
                self.assertIn(f'debugfs -R "dump {snapshot_path} ', runner)
        for analyzer_argument in (
            "thermal_rknn_starry_reference.py",
            "--embedded-runner",
            "--embedded-model",
            "--embedded-corpus",
            "--embedded-runtime",
            "--built-runner",
            "--source-commit",
            "--source-provenance captured-before-run",
            "--tracked-change-count",
            "--untracked-file-count",
        ):
            with self.subTest(analyzer_argument=analyzer_argument):
                self.assertIn(analyzer_argument, runner)
        self.assertIn("find . -type f", runner)
        self.assertIn("! -name checksums.sha256", runner)
        self.assertIn("checksum_partial=$(mktemp)", runner)
        self.assertIn(
            'mv -- "$checksum_partial" "$result_dir/checksums.sha256"',
            runner,
        )

    def test_board_flow_preserves_failures_from_every_phase(self) -> None:
        runner = RKNPU_RUNNER.read_text(encoding="utf-8")

        self.assertIn("automation-failure-status.txt", runner)
        cleanup = re.search(
            r"cleanup\(\)\s*\{(?P<body>.*?)\n\}",
            runner,
            re.DOTALL,
        )
        self.assertIsNotNone(cleanup)
        self.assertIn("write_checksums", cleanup.group("body"))
        self.assertIn("finished_at=", cleanup.group("body"))

    def test_host_filesystem_sync_marker_is_paced_for_a_lossy_uart(self) -> None:
        source = AXVISOR_HOST_COMMAND.read_text(encoding="utf-8")

        self.assertIn("HOST_FILESYSTEM_SYNCED_MARKER_COPIES: usize = 5", source)
        self.assertIn("HOST_FILESYSTEM_SYNCED_MARKER_INTERVAL_MS: u64 = 100", source)
        self.assertIn("write_host_filesystem_synced_markers_with_pause", source)
        self.assertRegex(
            source,
            re.compile(
                r"write_host_filesystem_synced_markers_with_pause\(.*?"
                r"thread::sleep\(\s*Duration::from_millis\(\s*"
                r"HOST_FILESYSTEM_SYNCED_MARKER_INTERVAL_MS\s*,?\s*\)\s*\)",
                re.DOTALL,
            ),
        )


if __name__ == "__main__":
    unittest.main()
