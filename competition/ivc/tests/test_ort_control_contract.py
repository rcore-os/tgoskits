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
HARVESTER = REPOSITORY_ROOT / "competition/ivc/orangepi/harvest-result.sh"
ANALYZER = REPOSITORY_ROOT / "competition/ivc/analyze_board.py"
AXVISOR_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/axvisor-orangepi-5-plus-ort-control-smoke.toml"
)
STARRY_CONFIG = (
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-ort-control-smoke.toml"
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

    def test_board_flow_harvests_and_independently_analyzes_ort_csv(self) -> None:
        runner = BOARD_RUNNER.read_text(encoding="utf-8")
        harvester = HARVESTER.read_text(encoding="utf-8")
        analyzer = ANALYZER.read_text(encoding="utf-8")

        for marker in (
            "ort-smoke",
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


if __name__ == "__main__":
    unittest.main()
