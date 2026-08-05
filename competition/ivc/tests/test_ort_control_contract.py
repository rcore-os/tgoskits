#!/usr/bin/env python3

import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
IVCPROTO_MANIFEST = REPOSITORY_ROOT / "tools/ivcproto/Cargo.toml"
IVCPROTO_BUILD = REPOSITORY_ROOT / "tools/ivcproto/build.rs"
ORT_BRIDGE = REPOSITORY_ROOT / "tools/ivcproto/csrc/ort_bridge.cpp"
ORT_ADAPTER = REPOSITORY_ROOT / "tools/ivcproto/src/ort.rs"
ORT_ROOTFS_BUILD = (
    REPOSITORY_ROOT / "competition/ivc/starry/build-ort-control-rootfs.sh"
)
STARRY_AUTORUN = REPOSITORY_ROOT / "competition/ivc/starry/autorun.sh"
BOARD_RUNNER = REPOSITORY_ROOT / "competition/ivc/run-orangepi-5-plus.sh"
ORT_STAGER = REPOSITORY_ROOT / "competition/ivc/stage-ort-control.sh"
HARVESTER = REPOSITORY_ROOT / "competition/ivc/orangepi/harvest-result.sh"
ANALYZER = REPOSITORY_ROOT / "competition/ivc/analyze_board.py"
CAMPAIGN_CONTRACT = REPOSITORY_ROOT / "competition/ivc/ort_campaign_contract.py"
CAMPAIGN_AGGREGATOR = REPOSITORY_ROOT / "competition/ivc/aggregate_ort_campaign.py"
CAMPAIGN_RUNNER = REPOSITORY_ROOT / "competition/ivc/run-ort-control-campaign.sh"
AXVISOR_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-ort-control-smoke.toml"
)
AXVISOR_FULL_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-ort-control.toml"
)
STARRY_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-ort-control-smoke.toml"
)
STARRY_FULL_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-ort-control.toml"
)


class OrtControlContractTests(unittest.TestCase):
    def test_ivcproto_links_ort_through_a_typed_bridge(self) -> None:
        manifest = IVCPROTO_MANIFEST.read_text(encoding="utf-8")
        build = IVCPROTO_BUILD.read_text(encoding="utf-8")
        bridge = ORT_BRIDGE.read_text(encoding="utf-8")
        adapter = ORT_ADAPTER.read_text(encoding="utf-8")

        self.assertIn('onnxruntime = ["std"]', manifest)
        self.assertIn("IVC_ORT_BRIDGE_LIB_DIR", build)
        self.assertIn("IVC_ORT_RUNTIME_LIB_DIR", build)
        self.assertIn('rustc-link-lib=static=ivc_ort_bridge', build)
        self.assertIn('extern "C" int ivc_ort_create', bridge)
        self.assertIn('extern "C" int ivc_ort_infer', bridge)
        self.assertIn('extern "C" int ivc_ort_destroy', bridge)
        self.assertIn('kExpectedRuntimeVersion = "1.25.0"', bridge)
        self.assertIn('kProvider = "CPUExecutionProvider"', bridge)
        self.assertIn('pub struct OrtController', adapter)

    def test_tensor_metadata_views_keep_their_type_info_owners_alive(self) -> None:
        bridge = ORT_BRIDGE.read_text(encoding="utf-8")

        self.assertIn(
            "const auto input_type = session.GetInputTypeInfo(0);", bridge
        )
        self.assertIn(
            "const auto output_type = session.GetOutputTypeInfo(0);", bridge
        )
        self.assertNotIn(
            "session.GetInputTypeInfo(0).GetTensorTypeAndShapeInfo()", bridge
        )
        self.assertNotIn(
            "session.GetOutputTypeInfo(0).GetTensorTypeAndShapeInfo()", bridge
        )

    def test_rootfs_builder_pins_and_audits_the_official_runtime(self) -> None:
        script = ORT_ROOTFS_BUILD.read_text(encoding="utf-8")

        self.assertIn("onnxruntime-linux-aarch64-1.25.0.tgz", script)
        self.assertIn(
            "849c04634e76446bbe0a92f67955a9641415c37f11930804066057bf9eadbd03",
            script,
        )
        self.assertIn(
            "e03801f70263a028207491471f09a17ed6a62b146568edada797483f8f8ec8d3",
            script,
        )
        self.assertIn("--features onnxruntime", script)
        self.assertIn("audit_dynamic_dependencies", script)
        self.assertIn("ivc_ort_model_sha256=", script)
        self.assertIn("ivc_ort_runtime_sha256=", script)
        self.assertIn("ivc_ort_controller_sha256=", script)

    def test_autorun_gates_and_persists_ort_control_evidence(self) -> None:
        script = STARRY_AUTORUN.read_text(encoding="utf-8")

        self.assertIn("native|rknn-npu|onnxruntime", script)
        self.assertNotIn("onnxruntime-backend-not-installed", script)
        self.assertIn("/opt/thermal-ort/thermal-4x6x1-v1.ort", script)
        self.assertIn("/var/lib/ivc/ort.csv", script)
        self.assertIn("validate_ort_evidence", script)
        self.assertIn("--ort-model \"$ivc_ort_model\"", script)
        self.assertIn("--ort-evidence \"$ivc_ort_evidence\"", script)
        self.assertIn("IVC-STARRY-ORT-RAW sha256=$validated_ort_sha256", script)

    def test_smoke_config_runs_starry_ort_control_with_the_zephyr_peer(self) -> None:
        axvisor = AXVISOR_CONFIG.read_text(encoding="utf-8")
        starry = STARRY_CONFIG.read_text(encoding="utf-8")

        self.assertIn("orangepi-5-plus-starry-smp2-ort-control-smoke.toml", axvisor)
        self.assertIn("orangepi-5-plus-zephyr-smoke.toml", axvisor)
        self.assertNotIn("rk3588-npu-handoff", axvisor)
        self.assertIn("starry-ivc-rootfs-ort-control-smoke.img", starry)
        self.assertIn('"virtio-net-starry"', starry)
        self.assertRegex(starry, r"(?m)^passthrough_devices\s*=\s*\[\s*\]$")

    def test_full_config_runs_1800_sample_ort_control_without_npu_handoff(self) -> None:
        axvisor = AXVISOR_FULL_CONFIG.read_text(encoding="utf-8")
        starry = STARRY_FULL_CONFIG.read_text(encoding="utf-8")
        runner = BOARD_RUNNER.read_text(encoding="utf-8")

        self.assertIn("orangepi-5-plus-starry-smp2-ort-control.toml", axvisor)
        self.assertIn("orangepi-5-plus-zephyr-smp1.toml", axvisor)
        self.assertNotIn("rk3588-npu-handoff", axvisor)
        self.assertIn("starry-ivc-rootfs-ort-control.img", starry)
        self.assertIn('expected_count=1800', runner)
        self.assertIn('result_image_name=ivc-on', runner)
        self.assertIn('ort-full)', runner)

    def test_profile_stager_requires_only_the_selected_ort_rootfs(self) -> None:
        runner = BOARD_RUNNER.read_text(encoding="utf-8")
        stager = ORT_STAGER.read_text(encoding="utf-8")

        self.assertIn('IVC_ORT_CONTROL_ROOTFS="$local_rootfs"', runner)
        self.assertIn(
            'selected_rootfs=${IVC_ORT_CONTROL_ROOTFS:?set IVC_ORT_CONTROL_ROOTFS}',
            stager,
        )
        self.assertIn('"$selected_rootfs"', stager)
        self.assertIn('"$(basename -- "$selected_rootfs")"', stager)

    def test_board_flow_harvests_and_independently_analyzes_ort_csv(self) -> None:
        runner = BOARD_RUNNER.read_text(encoding="utf-8")
        harvester = HARVESTER.read_text(encoding="utf-8")
        analyzer = ANALYZER.read_text(encoding="utf-8")

        for marker in (
            "ort-smoke",
            "ort-full",
            "ORANGEPI_IVC_ORT_CSV",
            "ort.csv.gz",
            "--ort-csv",
            "--expected-ort-model-sha256",
            "--expected-ort-runtime-version",
            "stage-ort-control.sh",
        ):
            with self.subTest(runner_marker=marker):
                self.assertIn(marker, runner)
        for marker in (
            "/var/lib/ivc/ort.csv",
            "/var/lib/ivc/ort.csv.sha256",
            "BOARD_GUEST_ORT_MANIFEST",
            "BOARD_ORT_RESULT_HARVESTED",
        ):
            with self.subTest(harvest_marker=marker):
                self.assertIn(marker, harvester)
        for marker in (
            "def parse_ort_samples(",
            "CPUExecutionProvider",
            "ORT CSV SHA-256 does not match",
            "ORT actuator does not match the controller raw CSV",
        ):
            with self.subTest(analyzer_marker=marker):
                self.assertIn(marker, analyzer)

    def test_full_campaign_is_preregistered_and_independently_aggregated(self) -> None:
        contract = CAMPAIGN_CONTRACT.read_text(encoding="utf-8")
        aggregator = CAMPAIGN_AGGREGATOR.read_text(encoding="utf-8")
        runner = CAMPAIGN_RUNNER.read_text(encoding="utf-8")

        for marker in (
            "run_count: int = EXPECTED_RUNS",
            "samples_per_run: int = EXPECTED_COUNT",
            '"replacement_runs_allowed": False',
            '"max_ort_wall_p99_ns"',
            '"startup_semantics": "fresh-board-reboot-and-new-ort-session"',
        ):
            with self.subTest(contract_marker=marker):
                self.assertIn(marker, contract)
        for marker in (
            "load_preregistration_evidence",
            "validate_deadline_contract",
            "validate_ort_timing_contract",
            '"formal_gate_passed": True',
        ):
            with self.subTest(aggregator_marker=marker):
                self.assertIn(marker, aggregator)
        for marker in (
            "ort-full",
            "--repeat 5",
            "preregistration.sha256",
            "campaign-summary.json",
            "campaign-checksums.sha256",
        ):
            with self.subTest(runner_marker=marker):
                self.assertIn(marker, runner)


if __name__ == "__main__":
    unittest.main()
