#!/usr/bin/env python3

import re
import unittest
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older
    import tomli as tomllib


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
VIRTIO_NET_DRIVER = REPOSITORY_ROOT / "drivers/ax-driver/src/virtio/net.rs"
STARRY_AUTORUN = REPOSITORY_ROOT / "competition/ivc/starry/autorun.sh"
STARRY_BUILD = REPOSITORY_ROOT / "competition/ivc/starry/build.sh"
STARRY_ROOTFS_BUILD = REPOSITORY_ROOT / "competition/ivc/starry/build-rootfs.sh"
ORANGEPI_STARRY_CONFIGS = (
    REPOSITORY_ROOT / "competition/ivc/config/orangepi-5-plus-starry-smp2.toml",
    REPOSITORY_ROOT
    / "competition/ivc/config/orangepi-5-plus-starry-smp2-smoke.toml",
)
ORANGEPI_STARRY_MANUAL_CONFIGS = (
    (
        REPOSITORY_ROOT / "competition/ivc/config/orangepi-5-plus-starry-smp2.toml",
        REPOSITORY_ROOT
        / "competition/ivc/config/orangepi-5-plus-starry-smp2-manual.toml",
        "starry-ivc-rootfs-manual.img",
    ),
    (
        REPOSITORY_ROOT
        / "competition/ivc/config/orangepi-5-plus-starry-smp2-smoke.toml",
        REPOSITORY_ROOT
        / "competition/ivc/config/orangepi-5-plus-starry-smp2-manual-smoke.toml",
        "starry-ivc-rootfs-manual-smoke.img",
    ),
)


class StarryGuestContractTests(unittest.TestCase):
    def test_rootfs_builder_exposes_orthogonal_profile_dimensions(self) -> None:
        script = STARRY_ROOTFS_BUILD.read_text(encoding="utf-8")

        for option in (
            "--profile",
            "--policy",
            "--backend",
            "--count",
            "--period-ms",
            "--output",
        ):
            with self.subTest(option=option):
                self.assertIn(option, script)
        self.assertIn("ivc_backend=%s", script)
        self.assertNotIn("printf 'ivc_mode=neural", script)

    def test_starry_build_produces_manual_and_neural_rootfs_images(self) -> None:
        script = STARRY_BUILD.read_text(encoding="utf-8")

        self.assertIn("--policy neural", script)
        self.assertIn("--policy manual", script)
        self.assertIn("starry-ivc-rootfs-manual.img", script)
        self.assertIn("starry-ivc-rootfs-manual-smoke.img", script)

    def test_starry_build_materializes_a_fresh_raw_kernel(self) -> None:
        script = STARRY_BUILD.read_text(encoding="utf-8")

        self.assertIn("built_elf=$workspace/target/", script)
        self.assertIn(
            'rustup run "$toolchain" llvm-objcopy --strip-all -O binary',
            script,
        )
        self.assertLess(
            script.index("xtask starry build"),
            script.index("llvm-objcopy --strip-all -O binary"),
        )
        self.assertLess(
            script.index("llvm-objcopy --strip-all -O binary"),
            script.index('install -m 0644 "$built_kernel"'),
        )

    def test_manual_guests_only_change_identity_and_rootfs_policy_image(self) -> None:
        for neural_path, manual_path, manual_image in ORANGEPI_STARRY_MANUAL_CONFIGS:
            with self.subTest(config=manual_path.name):
                with neural_path.open("rb") as source:
                    neural = tomllib.load(source)
                with manual_path.open("rb") as source:
                    manual = tomllib.load(source)

                self.assertIn(manual_image, manual["kernel"]["disk_path"])
                manual["base"]["name"] = neural["base"]["name"]
                manual["kernel"]["disk_path"] = neural["kernel"]["disk_path"]
                self.assertEqual(manual, neural)

    def test_autorun_records_the_selected_inference_backend(self) -> None:
        script = STARRY_AUTORUN.read_text(encoding="utf-8")

        self.assertRegex(script, r"(?m)^case \"\$\{ivc_backend:-\}\" in$")
        self.assertIn("backend=$ivc_backend", script)

    def test_starry_guest_persists_and_validates_raw_controller_samples(self) -> None:
        builder = STARRY_ROOTFS_BUILD.read_text(encoding="utf-8")
        autorun = STARRY_AUTORUN.read_text(encoding="utf-8")

        self.assertIn("ivc_raw_csv=/var/lib/ivc/raw.csv", builder)
        self.assertIn("/var/lib/ivc", builder)
        self.assertIn('--raw-csv "$ivc_raw_csv"', autorun)
        self.assertIn("expected_raw_lines=$((expected_samples + 1))", autorun)
        self.assertIn('"$BB" wc -l < "$raw_path"', autorun)
        self.assertIn(
            "IVC-STARRY-RAW path=$ivc_raw_csv samples=$ivc_count sha256=$raw_sha256",
            autorun,
        )
        self.assertIn("raw_manifest=$raw_path.sha256", autorun)
        self.assertIn(
            "printf '%s  %s\\n' \"$validated_raw_sha256\" \"$raw_path\" >\"$raw_manifest\"",
            autorun,
        )
        self.assertIn('"$BB" sync || fatal final-sync-failed', autorun)
        self.assertLess(
            autorun.index('"$BB" sync || fatal final-sync-failed'),
            autorun.index('echo "IVC-STARRY-DONE exit=$result"'),
        )

    def test_restart_profile_persists_phase_one_before_waiting_for_vm_reset(self) -> None:
        builder = STARRY_ROOTFS_BUILD.read_text(encoding="utf-8")
        autorun = STARRY_AUTORUN.read_text(encoding="utf-8")

        self.assertIn("none|error|restart", builder)
        self.assertIn(
            "fault profile must be 'none', 'error', or 'restart'",
            builder,
        )
        self.assertIn("ivc_restart_previous_session=286331153", builder)
        self.assertIn("ivc_restart_current_session=572662306", builder)
        self.assertIn("ivc_restart_first_count=20", builder)
        self.assertIn("/var/lib/ivc/restart-phase-1.done", autorun)
        self.assertIn("/var/lib/ivc/raw-before-reset.csv", autorun)
        self.assertIn("IVC-STARRY-RESTART-ARMED", autorun)
        self.assertIn("IVC-STARRY-RESTART-RESUME", autorun)
        self.assertIn(
            'restart_uart_sha256=$(printf \'%s\' "$restart_raw_sha256" | "$BB" cut -c1-12)',
            autorun,
        )
        self.assertIn("sha256=$restart_uart_sha256", autorun)
        self.assertIn('--fault-profile restart', autorun)
        self.assertIn('--restart-previous-session "$ivc_restart_previous_session"', autorun)
        self.assertLess(
            autorun.index('"$BB" sync || fatal restart-phase-sync-failed'),
            autorun.index('IVC-STARRY-RESTART-ARMED'),
        )

    def test_restart_raw_record_fits_the_shared_uart_budget(self) -> None:
        record = (
            "[guest-console:pl011-starry] "
            "IVC-STARRY-RESTART-RAW "
            "path=/var/lib/ivc/raw-before-reset.csv "
            "samples=20 sha256="
            + "a" * 12
        )

        self.assertLessEqual(len(record.encode("ascii")), 160)

    def test_virtio_net_driver_registers_an_fdt_mmio_probe(self) -> None:
        production_source = VIRTIO_NET_DRIVER.read_text(encoding="utf-8").split(
            "#[cfg(test)]", maxsplit=1
        )[0]

        self.assertRegex(
            production_source,
            re.compile(
                r"VIRTIO_NET_PROBE_KINDS.*?ProbeKind::Fdt\s*\{"
                r".*?compatibles:\s*&\[\"virtio,mmio\"\]"
                r".*?on_probe:\s*probe_fdt",
                re.DOTALL,
            ),
        )

    def test_autorun_does_not_require_linux_link_state_ioctls(self) -> None:
        script = STARRY_AUTORUN.read_text(encoding="utf-8")

        self.assertNotIn("ip link set eth0 down", script)
        self.assertNotIn("ip link set eth0 up", script)
        self.assertIn("ip addr add 10.0.0.1/24 dev eth0", script)

    def test_orangepi_guests_use_virtual_interrupt_delivery(self) -> None:
        for config_path in ORANGEPI_STARRY_CONFIGS:
            with self.subTest(config=config_path.name):
                config = config_path.read_text(encoding="utf-8")

                self.assertRegex(config, r"(?m)^passthrough_devices\s*=\s*\[\s*\]$")
                self.assertRegex(
                    config,
                    re.compile(r'(?m)^interrupt_mode\s*=\s*"emulated"$'),
                )
                self.assertRegex(
                    config,
                    re.compile(r"(?m)^aarch64_virtual_timer_irq\s*=\s*27$"),
                )


if __name__ == "__main__":
    unittest.main()
