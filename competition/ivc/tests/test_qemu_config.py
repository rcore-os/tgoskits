from __future__ import annotations

import json
import gzip
import os
import re
import subprocess
import tempfile
try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 and older
    import tomli as tomllib
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
QEMU_CONFIG = REPOSITORY_ROOT / "competition/ivc/config/qemu-aarch64.toml"
ORANGEPI_BOARD_CONFIGS = (
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus-smoke.toml",
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus.toml",
    REPOSITORY_ROOT
    / "competition/ivc/config/board-orangepi-5-plus-manual-smoke.toml",
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus-manual.toml",
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus-ack-loss.toml",
    REPOSITORY_ROOT / "competition/ivc/config/board-orangepi-5-plus-error.toml",
)
ORANGEPI_BOARD_SNAPSHOT_CONFIGS = (
    (ORANGEPI_BOARD_CONFIGS[0], "/home/orangepi/ivc-ns"),
    (ORANGEPI_BOARD_CONFIGS[1], "/home/orangepi/ivc-n"),
    (ORANGEPI_BOARD_CONFIGS[2], "/home/orangepi/ivc-ms"),
    (ORANGEPI_BOARD_CONFIGS[3], "/home/orangepi/ivc-m"),
    (ORANGEPI_BOARD_CONFIGS[4], "/home/orangepi/ivc-a"),
    (ORANGEPI_BOARD_CONFIGS[5], "/home/orangepi/ivc-e"),
)
LINUX_ACK_LOSS_CONFIG = (
    REPOSITORY_ROOT / "competition/ivc/config/linux-smp2-ack-loss.toml"
)
ZEPHYR_ACK_LOSS_CONFIG = (
    REPOSITORY_ROOT / "competition/ivc/config/zephyr-smp1-ack-loss.toml"
)
ZEPHYR_ACK_LOSS_CONF = REPOSITORY_ROOT / "competition/ivc/zephyr/ack-loss.conf"
ZEPHYR_BOARD_ACK_LOSS_CONF = (
    REPOSITORY_ROOT / "competition/ivc/zephyr/board-ack-loss.conf"
)
ZEPHYR_BOARD_ERROR_CONF = REPOSITORY_ROOT / "competition/ivc/zephyr/board-error.conf"
ZEPHYR_KCONFIG = REPOSITORY_ROOT / "competition/ivc/zephyr/Kconfig"
ZEPHYR_MAIN = REPOSITORY_ROOT / "competition/ivc/zephyr/src/main.c"
ZEPHYR_GITIGNORE = REPOSITORY_ROOT / "competition/ivc/zephyr/.gitignore"
IVCPROTO_BIN = REPOSITORY_ROOT / "tools/ivcproto/src/bin/ivcproto.rs"
ORANGEPI_RUN_SCRIPT = REPOSITORY_ROOT / "competition/ivc/run-orangepi-5-plus.sh"
ORANGEPI_STAGE_SCRIPT = REPOSITORY_ROOT / "competition/ivc/stage-orangepi-5-plus.sh"
AXVISOR_SHELL_BASE = REPOSITORY_ROOT / "os/axvisor/src/shell/command/base.rs"
AXVISOR_SHELL_HOST = REPOSITORY_ROOT / "os/axvisor/src/shell/command/host.rs"
AXVISOR_SHELL = REPOSITORY_ROOT / "os/axvisor/src/shell/mod.rs"
AXVM_VM_SOURCE = REPOSITORY_ROOT / "virtualization/axvm/src/vm/mod.rs"
ORANGEPI_MANUAL_BUILD_CONFIGS = (
    (
        REPOSITORY_ROOT
        / "competition/ivc/config/axvisor-orangepi-5-plus-smoke.toml",
        REPOSITORY_ROOT
        / "competition/ivc/config/axvisor-orangepi-5-plus-manual-smoke.toml",
        "competition/ivc/config/orangepi-5-plus-starry-smp2-manual-smoke.toml",
    ),
    (
        REPOSITORY_ROOT / "competition/ivc/config/axvisor-orangepi-5-plus.toml",
        REPOSITORY_ROOT
        / "competition/ivc/config/axvisor-orangepi-5-plus-manual.toml",
        "competition/ivc/config/orangepi-5-plus-starry-smp2-manual.toml",
    ),
)
ORANGEPI_AUTOMATION_SCRIPTS = (
    REPOSITORY_ROOT / "competition/ivc/orangepi/board-runner.sh",
    REPOSITORY_ROOT / "competition/ivc/orangepi/harvest-result.sh",
    REPOSITORY_ROOT / "competition/ivc/orangepi/prepare-service-dtb.sh",
    REPOSITORY_ROOT / "competition/ivc/orangepi/restore-linux.sh",
    REPOSITORY_ROOT / "competition/ivc/orangepi/serial-command.sh",
)

VALID_SMOKE_RUNNER = """#!/bin/sh
set -eu
: "${ORANGEPI_IVC_RAW_CSV:?}"
mkdir -p "$(dirname -- "$ORANGEPI_IVC_RAW_CSV")"
{
    echo 'sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,command_actuator_permille,status_actuator_permille,error_milli_c'
    sequence=1
    while [ "$sequence" -le 20 ]; do
        cycle_started_us=$(((sequence - 1) * 100000))
        command_sent_us=$((cycle_started_us + 10))
        response_completed_us=$((cycle_started_us + 110))
        printf '%s,%s,%s,%s,110,10,100,45000,44000,44000,500,500,1000\n' \
            "$sequence" "$cycle_started_us" "$command_sent_us" "$response_completed_us"
        sequence=$((sequence + 1))
    done
} >"$ORANGEPI_IVC_RAW_CSV"
raw_record=$(sha256sum "$ORANGEPI_IVC_RAW_CSV")
raw_sha256=${raw_record%% *}
cat <<EOF
[guest-console:pl011-starry] IVC-STARRY-BOOT mode=neural backend=native count=20 period_ms=100 vcpus=2
[guest-console:pl011-starry] IVC-STARRY-NET iface=eth0 mac=unknown ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1
[guest-console:pl011-starry] IVC-CONTROLLER-OUTCOME policy=neural sent=20 acknowledged=20 errors=0 timeouts=0
[guest-console:pl011-starry] IVC-CONTROLLER-RELIABILITY retransmissions=0 recoveries=0 success_percent=100.000
[guest-console:pl011-starry] IVC-CONTROLLER-FULL-LOOP p50_us=110 p95_us=110 p99_us=110 max_us=110
[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND p50_us=10 p95_us=10 p99_us=10 max_us=10
[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT p50_us=100 p95_us=100 p99_us=100 max_us=100 throughput_msg_s=9.995
[guest-console:pl011-starry] IVC-CONTROLLER-CONTROL rmse_milli_c=1000.000 iae_milli_c_s=2000.000 max_overshoot_milli_c=0
[guest-console:pl011-starry] IVC-STARRY-RAW path=/var/lib/ivc/raw.csv samples=20 sha256=$raw_sha256
[guest-console:pl011-zephyr] IVC-RTOS-RESULT profile=normal accepted=20 applied=20 duplicates=0 acks_dropped=0 status_sent=20 acks_sent=20 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal accepted=20 applied=20 duplicates=0 acks_dropped=0
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=normal accepted=20 applied=20 duplicates=0 acks_dropped=0
[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES status_sent=20 acks_sent=20 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES status_sent=20 acks_sent=20 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-POWEROFF accepted=20
[guest-console:pl011-zephyr] IVC-RTOS-POWEROFF accepted=20
[guest-console:pl011-starry] IVC-STARRY-DONE exit=0
AXVISOR_SNAPSHOT_SYNC_OK
BOARD_LINUX_RESTORED
BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 path=/home/orangepi/ivc-ns bytes=67108864 sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef fsck=clean
BOARD_GUEST_RAW_MANIFEST path=/var/lib/ivc/raw.csv samples=20 sha256=$raw_sha256
BOARD_RAW_RESULT_HARVESTED path=$ORANGEPI_IVC_RAW_CSV samples=20 sha256=$raw_sha256
BOARD_IDENTITY board_id=fake-rk3588 hostname=orangepi5plus cpu_temp_milli_c=42000
EOF
"""

VALID_ACK_LOSS_RUNNER = """#!/bin/sh
set -eu
: "${ORANGEPI_IVC_RAW_CSV:?}"
: "${ORANGEPI_IVC_RESULT_IMAGE:?}"
[ "${ORANGEPI_IVC_EXPECTED_COUNT:?}" = 100 ]
mkdir -p "$(dirname -- "$ORANGEPI_IVC_RAW_CSV")"
{
    echo 'sequence,cycle_started_us,command_sent_us,response_completed_us,full_loop_us,pre_send_us,transport_us,setpoint_milli_c,observed_milli_c,measured_milli_c,command_actuator_permille,status_actuator_permille,error_milli_c'
    sequence=1
    while [ "$sequence" -le 100 ]; do
        cycle_started_us=$(((sequence - 1) * 100000))
        command_sent_us=$((cycle_started_us + 10))
        response_completed_us=$((cycle_started_us + 110))
        printf '%s,%s,%s,%s,110,10,100,45000,44000,44000,500,500,1000\n' \
            "$sequence" "$cycle_started_us" "$command_sent_us" "$response_completed_us"
        sequence=$((sequence + 1))
    done
} >"$ORANGEPI_IVC_RAW_CSV"
raw_record=$(sha256sum "$ORANGEPI_IVC_RAW_CSV")
raw_sha256=${raw_record%% *}
cat <<EOF
[guest-console:pl011-starry] IVC-STARRY-BOOT mode=neural backend=native count=100 period_ms=100 vcpus=2
[guest-console:pl011-starry] IVC-STARRY-NET iface=eth0 mac=unknown ip=10.0.0.1/24 peer=10.0.0.2 udp_port=5500 segment=1
[guest-console:pl011-starry] IVC-CONTROLLER-OUTCOME policy=neural sent=100 acknowledged=100 errors=0 timeouts=0
[guest-console:pl011-starry] IVC-CONTROLLER-RELIABILITY retransmissions=20 recoveries=20 success_percent=100.000
[guest-console:pl011-starry] IVC-CONTROLLER-FULL-LOOP p50_us=110 p95_us=110 p99_us=110 max_us=110
[guest-console:pl011-starry] IVC-CONTROLLER-PRE-SEND p50_us=10 p95_us=10 p99_us=10 max_us=10
[guest-console:pl011-starry] IVC-CONTROLLER-TRANSPORT p50_us=100 p95_us=100 p99_us=100 max_us=100 throughput_msg_s=10.000
[guest-console:pl011-starry] IVC-CONTROLLER-CONTROL rmse_milli_c=1000.000 iae_milli_c_s=10000.000 max_overshoot_milli_c=0
[guest-console:pl011-starry] IVC-STARRY-RAW path=/var/lib/ivc/raw.csv samples=100 sha256=$raw_sha256
[guest-console:pl011-zephyr] IVC-RTOS-READY bind=10.0.0.2:5500 mac=52:54:00:00:00:02 window_bits=64 ack_loss_drop_every=5 expected_commands=100 exit_after_expected=1
EOF
sequence=5
duplicate=1
while [ "$sequence" -le 100 ]; do
    printf '[guest-console:pl011-zephyr] IVC-RTOS-INJECT drop_ack_seq=%s\n' "$sequence"
    printf '[guest-console:pl011-zephyr] IVC-RTOS-DUPLICATE seq=%s next_expected=%s duplicates=%s\n' \
        "$sequence" "$((sequence + 1))" "$duplicate"
    sequence=$((sequence + 5))
    duplicate=$((duplicate + 1))
done
cat <<EOF
[guest-console:pl011-zephyr] IVC-RTOS-OUTCOME profile=ack-loss accepted=100 applied=100 duplicates=20 acks_dropped=20
[guest-console:pl011-zephyr] IVC-RTOS-MESSAGES status_sent=120 acks_sent=100 errors_sent=0 protocol_errors=0
[guest-console:pl011-zephyr] IVC-RTOS-POWEROFF accepted=100
[guest-console:pl011-starry] IVC-STARRY-DONE exit=0
AXVISOR_SNAPSHOT_SYNC_OK
BOARD_LINUX_RESTORED
BOARD_RESULT_IMAGE_VALIDATED vm=1 index=0 path=$ORANGEPI_IVC_RESULT_IMAGE bytes=67108864 sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef fsck=clean
BOARD_GUEST_RAW_MANIFEST path=/var/lib/ivc/raw.csv samples=100 sha256=$raw_sha256
BOARD_RAW_RESULT_HARVESTED path=$ORANGEPI_IVC_RAW_CSV samples=100 sha256=$raw_sha256
BOARD_IDENTITY board_id=fake-rk3588 hostname=orangepi5plus cpu_temp_milli_c=42000
EOF
"""


def write_fake_board_runner(path: Path) -> None:
    path.write_text(VALID_SMOKE_RUNNER, encoding="utf-8")
    path.chmod(0o755)


def write_fake_ack_loss_runner(path: Path) -> None:
    path.write_text(VALID_ACK_LOSS_RUNNER, encoding="utf-8")
    path.chmod(0o755)


class QemuConfigContractTests(unittest.TestCase):
    def test_orangepi_default_runner_is_repository_local(self) -> None:
        entrypoint = ORANGEPI_RUN_SCRIPT.read_text(encoding="utf-8")

        self.assertIn("competition/ivc/orangepi/board-runner.sh", entrypoint)
        self.assertNotIn("orangepi-axvisor-board-run", entrypoint)
        for script_path in ORANGEPI_AUTOMATION_SCRIPTS:
            with self.subTest(script=script_path.name):
                self.assertTrue(script_path.is_file())
                script = script_path.read_text(encoding="utf-8")
                self.assertNotIn("/home/seven_wsl", script)
                self.assertNotIn(".local/bin/orangepi-", script)

    def test_repository_runner_requires_the_exact_sync_marker_before_power_cycle(
        self,
    ) -> None:
        runner = ORANGEPI_AUTOMATION_SCRIPTS[0].read_text(encoding="utf-8")

        self.assertIn("sync_marker_confirmed", runner)
        self.assertRegex(
            runner,
            re.compile(
                r"grep .*AXVISOR_SNAPSHOT_SYNC_OK.*?"
                r"sync_marker_confirmed=1",
                re.DOTALL,
            ),
        )
        self.assertRegex(
            runner,
            re.compile(
                r"sync_marker_confirmed.*?ORANGEPI_AXVISOR_SYNC_CONFIRMED=1",
                re.DOTALL,
            ),
        )
        self.assertIn("guest_marker_confirmed", runner)
        self.assertRegex(
            runner,
            re.compile(
                r"grep .*IVC-STARRY-DONE exit=0.*?guest_marker_confirmed=1",
                re.DOTALL,
            ),
        )

    def test_repository_runner_requires_a_fresh_result_snapshot(self) -> None:
        runner = ORANGEPI_AUTOMATION_SCRIPTS[0].read_text(encoding="utf-8")
        entrypoint = ORANGEPI_RUN_SCRIPT.read_text(encoding="utf-8")

        self.assertIn("ORANGEPI_IVC_RESULT_IMAGE", runner)
        self.assertIn("ORANGEPI_IVC_RESULT_IMAGE", entrypoint)
        self.assertIn("snapshot_marker_confirmed", runner)
        self.assertRegex(
            runner,
            re.compile(
                r"rm -f .*result_image.*?AXVISOR_SNAPSHOT_SYNC_OK.*?"
                r"snapshot_marker_confirmed=1",
                re.DOTALL,
            ),
        )
        self.assertRegex(
            runner,
            re.compile(r"guest_image.*?!=.*?result_image", re.DOTALL),
        )
        self.assertRegex(
            runner,
            re.compile(
                r"cargo xtask board connect.*?Allocated board session:.*?"
                r"rm -f .*result_image",
                re.DOTALL,
            ),
        )

    def test_board_runner_prefers_wsl_system_libclang_when_unset(self) -> None:
        runner = ORANGEPI_AUTOMATION_SCRIPTS[0].read_text(encoding="utf-8")

        self.assertIn("configure_system_libclang", runner)
        self.assertIn("/usr/lib/llvm-*/lib/libclang.so*", runner)
        self.assertIn("export LIBCLANG_PATH", runner)
        self.assertLess(
            runner.index("\nconfigure_system_libclang\n"),
            runner.index('"${cargo_command[@]}" xtask axvisor board'),
        )

    def test_board_runner_restores_the_lockfile_after_a_local_cargo_patch(self) -> None:
        runner = ORANGEPI_AUTOMATION_SCRIPTS[0].read_text(encoding="utf-8")

        self.assertIn("cargo_lock_backup", runner)
        self.assertIn("restore_cargo_lock", runner)
        self.assertRegex(
            runner,
            re.compile(
                r"backup_cargo_lock.*?cargo_command.*?--config.*?"
                r"wait \"\$runner_pid\".*?restore_cargo_lock",
                re.DOTALL,
            ),
        )

    def test_board_profiles_snapshot_the_matching_volatile_disk(self) -> None:
        for config_path, result_path in ORANGEPI_BOARD_SNAPSHOT_CONFIGS:
            with self.subTest(config=config_path.name), config_path.open("rb") as source:
                config = tomllib.load(source)

            self.assertLessEqual(
                len(config["shell_init_cmd"].encode("utf-8")) + 1,
                32,
                "the command and trailing newline must fit one UART RX FIFO",
            )
            self.assertTrue(config["shell_init_cmd"].startswith("ss 1 0 "))
            self.assertEqual(
                config["shell_init_cmd"],
                f"ss 1 0 {result_path}",
            )
            self.assertEqual(config["shell_prefix"], "AXVISOR_SHELL_READY")
            self.assertEqual(config["board_type"], "OrangePi-5-Plus")
            self.assertEqual(len(config["success_regex"]), 1)
            success = re.compile(config["success_regex"][0])
            self.assertEqual(
                config["success_regex"][0],
                r"(?m)^AXVISOR_SNAPSHOT_SYNC_OK\r*$",
            )
            guest_done = "[guest-console:pl011-starry] IVC-STARRY-DONE exit=0\n"
            snapshot_done = "AXVISOR_SNAPSHOT_SYNC_OK\n"
            host_synced = "AXVISOR_HOST_FILESYSTEM_SYNCED\n"
            self.assertIsNone(success.search(guest_done))
            self.assertIsNone(success.search(guest_done + host_synced))
            self.assertIsNotNone(success.search(snapshot_done))
            self.assertIsNotNone(success.search(guest_done + snapshot_done))
            self.assertTrue(
                any(
                    "AXVISOR_VM_BLOCK_SNAPSHOT_FAILED" in pattern
                    for pattern in config["fail_regex"]
                )
            )

    def test_repository_runner_harvests_starry_raw_samples_after_linux_restore(
        self,
    ) -> None:
        runner = ORANGEPI_AUTOMATION_SCRIPTS[0].read_text(encoding="utf-8")
        harvest = ORANGEPI_AUTOMATION_SCRIPTS[1].read_text(encoding="utf-8")
        entrypoint = ORANGEPI_RUN_SCRIPT.read_text(encoding="utf-8")

        self.assertIn("harvest-result.sh", runner)
        self.assertIn("ORANGEPI_IVC_RAW_CSV", runner)
        self.assertRegex(
            runner,
            re.compile(r"restore_status.*?harvest_result", re.DOTALL),
        )
        self.assertIn("cargo xtask board connect", harvest)
        self.assertIn("debugfs", harvest)
        self.assertNotIn("dump -p ", harvest)
        self.assertIn("e2fsck", harvest)
        self.assertIn("BOARD_RESULT_IMAGE_VALIDATED", harvest)
        self.assertIn("sha256sum", harvest)
        self.assertIn("/var/lib/ivc/raw.csv", harvest)
        self.assertIn("/var/lib/ivc/raw.csv.sha256", harvest)
        self.assertIn("BOARD_GUEST_RAW_MANIFEST", harvest)
        self.assertIn("sha256sum", harvest)
        self.assertIn("ORANGEPI_IVC_RESULT_IMAGE", harvest)
        self.assertNotIn("ORANGEPI_IVC_GUEST_IMAGE", harvest)
        self.assertIn("ORANGEPI_IVC_GUEST_IMAGE", entrypoint)
        self.assertIn("ORANGEPI_IVC_RESULT_IMAGE", entrypoint)
        self.assertIn("ORANGEPI_IVC_RAW_CSV", entrypoint)

    def test_linux_restore_can_use_windows_python_for_smart_plug_control(
        self,
    ) -> None:
        restore = ORANGEPI_AUTOMATION_SCRIPTS[3].read_text(encoding="utf-8")

        self.assertIn("python.exe", restore)
        self.assertIn("wslpath -w", restore)
        self.assertIn("run_power_tool status", restore)
        self.assertIn("run_power_tool cycle --yes", restore)
        self.assertIn("sync && echo BOARD_PRE_CYCLE_SYNC_DONE", restore)
        self.assertNotIn("sync; echo BOARD_PRE_CYCLE_SYNC_DONE", restore)
        self.assertIn('"$serial_command" sync-host', restore)
        self.assertNotIn('"$serial_command" shutdown', restore)

    def test_axvisor_repeats_the_host_sync_marker_for_the_shared_uart(self) -> None:
        source = AXVISOR_SHELL_HOST.read_text(encoding="utf-8")

        self.assertIn("HOST_FILESYSTEM_SYNCED_MARKER_COPIES: usize = 3", source)
        self.assertIn("write_host_filesystem_synced_markers", source)
        self.assertRegex(
            source,
            re.compile(
                r"shutdown_host_filesystems.*?write_host_filesystem_synced_markers",
                re.DOTALL,
            ),
        )

    def test_axvisor_repeats_the_snapshot_marker_for_the_shared_uart(self) -> None:
        source = AXVISOR_SHELL_HOST.read_text(encoding="utf-8")
        snapshot_flow = source.split("fn snapshot_and_sync", maxsplit=1)[1].split(
            "fn parse_snapshot_request", maxsplit=1
        )[0]

        self.assertIn("SNAPSHOT_SYNCED_MARKER_COPIES: usize = 5", source)
        self.assertIn('SNAPSHOT_SYNCED_MARKER: &str = "AXVISOR_SNAPSHOT_SYNC_OK"', source)
        self.assertIn("print_snapshot_synced_markers", source)
        self.assertIn("flush_host_filesystems()?", snapshot_flow)
        self.assertNotIn("shutdown_host_filesystems", snapshot_flow)
        self.assertNotIn("synchronize_host_filesystems", snapshot_flow)
        self.assertLess(
            snapshot_flow.index("persist_block_snapshot"),
            snapshot_flow.index("flush_host_filesystems()?"),
        )
        self.assertLess(
            snapshot_flow.index("flush_host_filesystems()?"),
            snapshot_flow.index("print_snapshot_synced_markers"),
        )

    def test_axvisor_repeats_the_shell_ready_marker_for_the_shared_uart(self) -> None:
        source = AXVISOR_SHELL.read_text(encoding="utf-8")

        copies_match = re.search(
            r"SHELL_READY_MARKER_COPIES:\s*usize\s*=\s*([0-9]+)", source
        )
        self.assertIsNotNone(copies_match)
        self.assertGreaterEqual(int(copies_match.group(1)), 3)
        self.assertIn('SHELL_READY_MARKER: &str = "AXVISOR_SHELL_READY"', source)
        interval_match = re.search(
            r"SHELL_READY_MARKER_INTERVAL_MS:\s*u64\s*=\s*([0-9]+)", source
        )
        self.assertIsNotNone(interval_match)
        self.assertGreaterEqual(int(interval_match.group(1)), 50)
        self.assertIn("write_shell_ready_markers", source)
        self.assertIn(
            "thread::sleep(Duration::from_millis(SHELL_READY_MARKER_INTERVAL_MS))",
            source,
        )
        self.assertRegex(
            source,
            re.compile(
                r"Welcome to AxVisor Shell!.*?write_shell_ready_markers.*?print_prompt",
                re.DOTALL,
            ),
        )

    def test_axvisor_snapshot_sync_command_does_not_exit_the_host(self) -> None:
        source = AXVISOR_SHELL_HOST.read_text(encoding="utf-8")
        vm_source = AXVM_VM_SOURCE.read_text(encoding="utf-8")

        self.assertIn('"snapshot-sync".to_string()', source)
        self.assertIn('"ss".to_string()', source)
        self.assertIn('"sync-host".to_string()', source)
        self.assertIn("snapshot_virtio_block_backing", source)
        self.assertIn("AXVISOR_VM_BLOCK_SNAPSHOT", source)
        self.assertNotIn("process::exit", source)
        self.assertRegex(
            source,
            re.compile(
                r"persist_block_snapshot.*?shutdown_host_filesystems.*?"
                r"write_host_filesystem_synced_markers",
                re.DOTALL,
            ),
        )
        self.assertIn("pub fn snapshot_virtio_block_backing", vm_source)
        self.assertRegex(
            vm_source,
            re.compile(
                r"snapshot_virtio_block_backing.*?ensure_block_snapshot_status",
                re.DOTALL,
            ),
        )

    def test_axvisor_persistence_writer_syncs_bounded_chunks(self) -> None:
        source = AXVISOR_SHELL_HOST.read_text(encoding="utf-8")

        chunk_match = re.search(
            r"SNAPSHOT_WRITE_CHUNK_BYTES:\s*usize\s*=\s*([^;]+);", source
        )
        self.assertIsNotNone(chunk_match)
        chunk_factors = [
            int(factor.strip().replace("_", ""))
            for factor in chunk_match.group(1).split("*")
        ]
        chunk_bytes = 1
        for factor in chunk_factors:
            chunk_bytes *= factor
        self.assertGreater(chunk_bytes, 0)
        self.assertLessEqual(chunk_bytes, 1024 * 1024)
        self.assertIn("persist_bytes_atomically(output_path, snapshot)", source)
        self.assertRegex(
            source,
            re.compile(
                r"for chunk in contents\.chunks\(SNAPSHOT_WRITE_CHUNK_BYTES\).*?"
                r"write_all\(chunk\).*?sync_host_filesystems\(\)",
                re.DOTALL,
            ),
        )

    def test_axvisor_host_trace_uses_bounded_persistence(self) -> None:
        source = AXVISOR_SHELL_HOST.read_text(encoding="utf-8")
        trace_flow = source.split("fn persist_host_rt_trace", maxsplit=1)[1].split(
            "fn write_host_rt_trace", maxsplit=1
        )[0]

        self.assertIn("let mut serialized = Vec::new()", trace_flow)
        self.assertIn("write_host_rt_trace(&mut serialized, trace)?", trace_flow)
        self.assertIn(
            "persist_bytes_atomically(output_path, &serialized)?", trace_flow
        )
        self.assertNotIn("write_host_rt_trace(&mut output, trace)?", trace_flow)

    def test_manual_build_configs_only_select_the_manual_starry_guest(self) -> None:
        for neural_path, manual_path, manual_guest in ORANGEPI_MANUAL_BUILD_CONFIGS:
            with self.subTest(config=manual_path.name):
                with neural_path.open("rb") as source:
                    neural = tomllib.load(source)
                with manual_path.open("rb") as source:
                    manual = tomllib.load(source)

                self.assertEqual(manual["vm_configs"][0], manual_guest)
                manual["vm_configs"][0] = neural["vm_configs"][0]
                self.assertEqual(manual, neural)

    def test_board_entrypoint_and_staging_cover_all_rootfs_profiles(self) -> None:
        entrypoint = ORANGEPI_RUN_SCRIPT.read_text(encoding="utf-8")
        staging = ORANGEPI_STAGE_SCRIPT.read_text(encoding="utf-8")

        self.assertIn("manual-smoke", entrypoint)
        self.assertIn("manual-full", entrypoint)
        self.assertIn("starry-ivc-rootfs-manual-smoke.img", staging)
        self.assertIn("starry-ivc-rootfs-manual.img", staging)
        self.assertIn("fault-ack-loss", entrypoint)
        self.assertIn("starry-ivc-rootfs-ack-loss.img", staging)
        self.assertIn("fault-error", entrypoint)
        self.assertIn("starry-ivc-rootfs-error.img", staging)

    def test_success_waits_for_complete_linux_result_line(self) -> None:
        with QEMU_CONFIG.open("rb") as source:
            config = tomllib.load(source)

        self.assertEqual(len(config["success_regex"]), 1)
        success = re.compile(config["success_regex"][0])
        failure_patterns = [re.compile(pattern) for pattern in config["fail_regex"]]
        prefix = (
            "IVC-CONTROLLER-RESULT policy=neural sent=1800 acknowledged=1800 "
            "errors=0 timeouts=0"
        )

        self.assertIsNone(success.search(prefix))
        self.assertIsNotNone(success.search("IVC-LINUX-DONE exit=0\n"))
        self.assertFalse(any(pattern.search(prefix) for pattern in failure_patterns))
        self.assertTrue(
            any(
                pattern.search(prefix.replace("errors=0", "errors=1"))
                for pattern in failure_patterns
            )
        )

    def test_orangepi_success_requires_a_complete_line_with_serial_crlf(self) -> None:
        completed = "[guest-console:pl011-starry] IVC-STARRY-DONE exit=0"

        for config_path, result_name in ORANGEPI_BOARD_SNAPSHOT_CONFIGS:
            with self.subTest(config=config_path.name), config_path.open("rb") as source:
                config = tomllib.load(source)
            self.assertEqual(len(config["success_regex"]), 1)
            success = re.compile(config["success_regex"][0])

            self.assertIsNone(success.search(completed))
            for line_ending in ("\n", "\r\n", "\r\r\n"):
                with self.subTest(line_ending=repr(line_ending)):
                    complete_capture = f"AXVISOR_SNAPSHOT_SYNC_OK{line_ending}"
                    self.assertIsNotNone(success.search(complete_capture))

            failure_patterns = [re.compile(pattern) for pattern in config["fail_regex"]]
            self.assertTrue(
                any(
                    pattern.search(
                        "Unhandled synchronous exception from current EL: TrapFrame"
                    )
                    for pattern in failure_patterns
                )
            )

    def test_orangepi_runner_retains_and_analyzes_the_console_capture(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            temporary = Path(temporary_dir)
            runner = temporary / "fake-board-runner"
            result_dir = temporary / "results"
            write_fake_board_runner(runner)
            environment = {
                **os.environ,
                "ORANGEPI_AXVISOR_RUNNER": str(runner),
                "ORANGEPI_AXVISOR_HOST_ROOT": "PARTUUID=test-root",
                "ORANGEPI_IVC_RESULT_DIR": str(result_dir),
            }

            completed = subprocess.run(
                ["bash", str(ORANGEPI_RUN_SCRIPT), "smoke"],
                cwd=REPOSITORY_ROOT,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            summary = json.loads(
                (result_dir / "smoke-summary.json").read_text(encoding="utf-8")
            )
            self.assertEqual(summary["controller"]["acknowledged"], 20)
            self.assertEqual(summary["raw_samples"]["sample_count"], 20)
            self.assertTrue(summary["lifecycle"]["board_linux_restored"])

    def test_orangepi_runner_creates_one_structured_result_per_repeat(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            temporary = Path(temporary_dir)
            runner = temporary / "fake-board-runner"
            result_dir = temporary / "results"
            write_fake_board_runner(runner)
            environment = {
                **os.environ,
                "ORANGEPI_AXVISOR_RUNNER": str(runner),
                "ORANGEPI_AXVISOR_HOST_ROOT": "PARTUUID=test-root",
            }

            completed = subprocess.run(
                [
                    "bash",
                    str(ORANGEPI_RUN_SCRIPT),
                    "--profile",
                    "smoke",
                    "--repeat",
                    "2",
                    "--board",
                    "OrangePi-5-Plus",
                    "--result-dir",
                    str(result_dir),
                    "--timeout",
                    "30",
                    "--restore-linux",
                ],
                cwd=REPOSITORY_ROOT,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            for run_id in ("run-001", "run-002"):
                with self.subTest(run_id=run_id):
                    run_dir = result_dir / "smoke" / run_id
                    self.assertTrue((run_dir / "console.log").is_file())
                    self.assertTrue((run_dir / "summary.json").is_file())
                    self.assertTrue((run_dir / "raw.csv").is_file())
                    self.assertTrue((run_dir / "raw.csv.gz").is_file())
                    self.assertTrue((run_dir / "console.log.gz").is_file())
                    self.assertTrue((run_dir / "metadata.json").is_file())
                    self.assertTrue((run_dir / "checksums.sha256").is_file())
                    metadata = json.loads(
                        (run_dir / "metadata.json").read_text(encoding="utf-8")
                    )
                    self.assertEqual(metadata["run"]["profile"], "smoke")
                    self.assertEqual(metadata["run"]["run_id"], run_id)
                    self.assertEqual(metadata["run"]["board_type"], "OrangePi-5-Plus")
                    self.assertEqual(metadata["run"]["exit_status"], 0)
                    self.assertEqual(metadata["run"]["repeat_count"], 2)
                    self.assertEqual(metadata["board"]["id"], "fake-rk3588")
                    self.assertEqual(metadata["board"]["cpu_temp_milli_c"], 42_000)
                    self.assertIn("starry_kernel", metadata["inputs"])
                    self.assertIn("starry_dtb", metadata["inputs"])
                    self.assertIn("rootfs", metadata["inputs"])
                    self.assertIn("artifact", metadata["model"])
                    self.assertEqual(
                        metadata["outputs"]["raw_csv"]["path"],
                        str(run_dir / "raw.csv.gz"),
                    )
                    self.assertEqual(
                        metadata["result"]["sample_count"], 20
                    )
                    with gzip.open(run_dir / "raw.csv.gz", "rt", encoding="utf-8") as source:
                        self.assertEqual(len(source.readlines()), 21)
                    checksums = (run_dir / "checksums.sha256").read_text(
                        encoding="utf-8"
                    )
                    self.assertIn("raw.csv.gz", checksums)
                    self.assertIn("console.log.gz", checksums)

    def test_orangepi_ack_loss_runner_emits_strict_recovery_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_dir:
            temporary = Path(temporary_dir)
            runner = temporary / "fake-ack-loss-board-runner"
            result_dir = temporary / "results"
            write_fake_ack_loss_runner(runner)
            environment = {
                **os.environ,
                "ORANGEPI_AXVISOR_RUNNER": str(runner),
                "ORANGEPI_AXVISOR_HOST_ROOT": "PARTUUID=test-root",
            }

            completed = subprocess.run(
                [
                    "bash",
                    str(ORANGEPI_RUN_SCRIPT),
                    "--profile",
                    "fault-ack-loss",
                    "--result-dir",
                    str(result_dir),
                ],
                cwd=REPOSITORY_ROOT,
                env=environment,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(completed.returncode, 0, completed.stderr)
            run_dir = result_dir / "fault-ack-loss" / "run-001"
            summary = json.loads(
                (run_dir / "summary.json").read_text(encoding="utf-8")
            )
            metadata = json.loads(
                (run_dir / "metadata.json").read_text(encoding="utf-8")
            )
            self.assertEqual(summary["profile"], "ack-loss")
            self.assertEqual(summary["controller"]["retransmissions"], 20)
            self.assertEqual(summary["rtos"]["acks_dropped"], 20)
            self.assertEqual(summary["rtos"]["duplicates"], 20)
            self.assertEqual(summary["rtos"]["injected_sequences"], list(range(5, 101, 5)))
            self.assertEqual(metadata["run"]["profile"], "fault-ack-loss")

    def test_ack_loss_guest_configs_pin_the_100_command_fault_campaign(self) -> None:
        with LINUX_ACK_LOSS_CONFIG.open("rb") as source:
            linux = tomllib.load(source)
        with ZEPHYR_ACK_LOSS_CONFIG.open("rb") as source:
            zephyr = tomllib.load(source)

        self.assertEqual(linux["base"]["cpu_num"], 2)
        self.assertEqual(linux["base"]["phys_cpu_sets"], [0x2, 0x4])
        self.assertIn("ivc.mode=neural", linux["kernel"]["cmdline"])
        self.assertIn("ivc.count=100", linux["kernel"]["cmdline"])
        self.assertIn("ivc.period_ms=100", linux["kernel"]["cmdline"])
        self.assertEqual(zephyr["base"]["phys_cpu_sets"], [0x1])
        self.assertEqual(
            zephyr["kernel"]["kernel_path"],
            "../zephyr/build-ack-loss/zephyr/zephyr.bin",
        )
        self.assertEqual(zephyr["devices"]["emu_devices"][0][4:], [0xE2, [2, 1, 1]])

    def test_ack_loss_build_overlay_is_explicit_and_default_remains_off(self) -> None:
        fault_config = ZEPHYR_ACK_LOSS_CONF.read_text(encoding="utf-8")
        board_fault_config = ZEPHYR_BOARD_ACK_LOSS_CONF.read_text(encoding="utf-8")
        kconfig = ZEPHYR_KCONFIG.read_text(encoding="utf-8")
        gitignore = ZEPHYR_GITIGNORE.read_text(encoding="utf-8").splitlines()

        self.assertIn("CONFIG_IVC_DROP_ACK_EVERY=5", fault_config)
        self.assertIn("CONFIG_IVC_EXPECTED_COMMANDS=100", fault_config)
        self.assertIn("CONFIG_IVC_DROP_ACK_EVERY=5", board_fault_config)
        self.assertIn("CONFIG_IVC_EXPECTED_COMMANDS=100", board_fault_config)
        self.assertIn("CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS=y", board_fault_config)
        self.assertIn("CONFIG_POWEROFF=y", board_fault_config)
        self.assertRegex(
            kconfig,
            r"(?s)config IVC_DROP_ACK_EVERY.*?default 0",
        )
        self.assertRegex(
            kconfig,
            r"(?s)config IVC_EXPECTED_COMMANDS.*?default 0",
        )
        self.assertIn("/build-ack-loss/", gitignore)
        self.assertIn("/build-board-ack-loss/", gitignore)

    def test_orangepi_ack_loss_profile_pins_matching_fault_artifacts(self) -> None:
        paths = (
            REPOSITORY_ROOT
            / "competition/ivc/config/axvisor-orangepi-5-plus-ack-loss.toml",
            REPOSITORY_ROOT
            / "competition/ivc/config/orangepi-5-plus-starry-smp2-ack-loss.toml",
            REPOSITORY_ROOT
            / "competition/ivc/config/orangepi-5-plus-zephyr-ack-loss.toml",
        )
        with paths[0].open("rb") as source:
            build = tomllib.load(source)
        with paths[1].open("rb") as source:
            starry = tomllib.load(source)
        with paths[2].open("rb") as source:
            zephyr = tomllib.load(source)

        self.assertEqual(
            build["vm_configs"],
            [
                paths[1].relative_to(REPOSITORY_ROOT).as_posix(),
                paths[2].relative_to(REPOSITORY_ROOT).as_posix(),
            ],
        )
        self.assertEqual(
            starry["kernel"]["disk_path"],
            "/home/orangepi/axvisor-guest/starry-ivc-rootfs-ack-loss.img",
        )
        self.assertEqual(
            zephyr["kernel"]["kernel_path"],
            "../zephyr/build-board-ack-loss/zephyr/zephyr.bin",
        )
        self.assertEqual(starry["base"]["phys_cpu_sets"], [0x2, 0x4])
        self.assertEqual(zephyr["base"]["phys_cpu_sets"], [0x1])

    def test_orangepi_error_profile_pins_five_error_responses(self) -> None:
        paths = (
            REPOSITORY_ROOT
            / "competition/ivc/config/axvisor-orangepi-5-plus-error.toml",
            REPOSITORY_ROOT
            / "competition/ivc/config/orangepi-5-plus-starry-smp2-error.toml",
            REPOSITORY_ROOT
            / "competition/ivc/config/orangepi-5-plus-zephyr-error.toml",
        )
        with paths[0].open("rb") as source:
            build = tomllib.load(source)
        with paths[1].open("rb") as source:
            starry = tomllib.load(source)
        with paths[2].open("rb") as source:
            zephyr = tomllib.load(source)
        board_overlay = ZEPHYR_BOARD_ERROR_CONF.read_text(encoding="utf-8")
        kconfig = ZEPHYR_KCONFIG.read_text(encoding="utf-8")

        self.assertEqual(
            build["vm_configs"],
            [
                paths[1].relative_to(REPOSITORY_ROOT).as_posix(),
                paths[2].relative_to(REPOSITORY_ROOT).as_posix(),
            ],
        )
        self.assertEqual(
            starry["kernel"]["disk_path"],
            "/home/orangepi/axvisor-guest/starry-ivc-rootfs-error.img",
        )
        self.assertEqual(
            zephyr["kernel"]["kernel_path"],
            "../zephyr/build-board-error/zephyr/zephyr.bin",
        )
        self.assertIn("CONFIG_IVC_EXPECTED_COMMANDS=100", board_overlay)
        self.assertIn("CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS=5", board_overlay)
        self.assertIn("CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS=y", board_overlay)
        self.assertRegex(
            kconfig,
            r"(?s)config IVC_EXPECTED_PROTOCOL_ERRORS.*?default 0",
        )

    def test_error_profile_replays_ready_on_the_first_observable_datagram(self) -> None:
        source = ZEPHYR_MAIN.read_text(encoding="utf-8")

        self.assertIn("report_ready();", source)
        self.assertIn("replay_ready_if_needed(server);", source)
        self.assertIn("bool ready_replayed;", source)

    def test_error_profile_replays_both_verified_fault_detail_sets(self) -> None:
        zephyr_source = ZEPHYR_MAIN.read_text(encoding="utf-8")
        controller_source = IVCPROTO_BIN.read_text(encoding="utf-8")

        self.assertIn("struct ivc_error_evidence", zephyr_source)
        self.assertIn("replay_error_evidence_if_complete(server);", zephyr_source)
        self.assertIn("replay_verified_error_fault_records()?;", controller_source)

    def test_error_profile_settles_shared_uart_before_terminal_result(self) -> None:
        controller_source = IVCPROTO_BIN.read_text(encoding="utf-8")

        self.assertRegex(
            controller_source,
            r"(?s)replay_verified_error_fault_records\(\)\?;\s*"
            r"std::thread::sleep\(ERROR_FAULT_RESULT_SETTLE\);\s*"
            r"for _ in 0\.\.ERROR_FAULT_RESULT_RECORD_COPIES",
        )
        self.assertIn("ERROR_FAULT_RESULT_RECORD_PAUSE", controller_source)


if __name__ == "__main__":
    unittest.main()
