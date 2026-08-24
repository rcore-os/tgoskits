from pathlib import Path


ROOT = Path(__file__).resolve().parent
BUILD_SCRIPT = (ROOT / "build-zephyr-task2.sh").read_text()
BOARD_OVERLAY = (
    ROOT / "zephyr-task2/atk-dlrk3588-axvisor.overlay"
)
TASK2_CMAKE = (ROOT / "zephyr-task2/CMakeLists.txt").read_text()
TASK2_CONFIG = (ROOT / "zephyr-task2/prj.conf").read_text()
PERIODIC_SOURCE = ROOT / "zephyr-task2/src/periodic.c"
CONSOLE_SOURCE = (ROOT / "zephyr-task2/src/console.c").read_text()
TELEMETRY_SOURCE = (ROOT / "zephyr-task2/src/telemetry.c").read_text()


def test_task2_build_accepts_a_physical_guest_overlay() -> None:
    assert 'extra_overlay="${TASK2_ZEPHYR_EXTRA_OVERLAY:-}"' in BUILD_SCRIPT
    assert 'overlay_files=("$device_overlay" "$memory_overlay")' in BUILD_SCRIPT
    assert 'overlay_files+=("$(realpath "$extra_overlay")")' in BUILD_SCRIPT
    assert 'printf \'extra_overlay = "%s"\\n\'' in BUILD_SCRIPT


def test_standalone_zephyr_checkout_gets_an_empty_module_environment() -> None:
    assert 'mkdir -p "$build_dir/Kconfig"' in BUILD_SCRIPT
    assert 'set(kconfig_env_dirs)' in BUILD_SCRIPT


def test_relative_output_paths_are_canonicalized_before_cmake() -> None:
    canonicalize = 'out_dir="$(realpath "$out_dir")"'
    overlay = 'memory_overlay="$out_dir/memory.overlay"'

    assert canonicalize in BUILD_SCRIPT
    assert 'build_dir="$(realpath "$build_dir")"' in BUILD_SCRIPT
    assert BUILD_SCRIPT.index(canonicalize) < BUILD_SCRIPT.index(overlay)


def test_task2_slot_metadata_matches_the_selected_virtio_window() -> None:
    assert 'fdt_path="/virtio_mmio@b000000"' in BUILD_SCRIPT
    assert "host_hwirq=0" in BUILD_SCRIPT
    assert "guest_irq=32" in BUILD_SCRIPT
    assert 'fdt_path="/virtio_mmio@a003c00"' in BUILD_SCRIPT
    assert "guest_irq=78" in BUILD_SCRIPT
    assert 'printf \'guest_irq = %s\\n\' "$guest_irq"' in BUILD_SCRIPT


def test_switch_overlay_matches_axvisor_aarch64_auto_resources() -> None:
    overlay = (ROOT / "zephyr-task2/app.overlay.switch").read_text()

    assert "virtio_mmio@b000000" in overlay
    assert "0x0b000000" in overlay
    assert "interrupts = <0x0 0x0 0x2 0xa0>;" in overlay


def test_physical_overlay_uses_the_rk3588_guest_gic_contract() -> None:
    overlay = BOARD_OVERLAY.read_text()

    assert "0xfe600000" in overlay
    assert "0xfe680000" in overlay
    assert '&its {' in overlay
    assert 'status = "disabled";' in overlay


def test_task2_image_also_builds_the_task1_periodic_probe() -> None:
    assert PERIODIC_SOURCE.is_file()
    assert "src/periodic.c" in TASK2_CMAKE
    assert "RT_START_GATED=1" in TASK2_CMAKE
    assert "RT_DUMP_GATED=1" in TASK2_CMAKE
    assert "RT_SAMPLE_COUNT" in TASK2_CMAKE
    assert 'periodic_probe = "enabled"' in BUILD_SCRIPT
    assert 'periodic_sample_count = "%s"' in BUILD_SCRIPT
    assert "CONFIG_SYS_CLOCK_TICKS_PER_SEC=1000" in TASK2_CONFIG
    assert "CONFIG_NET_QEMU_SLIP=y" in TASK2_CONFIG
    assert "CONFIG_NET_SLIP_TAP=n" in TASK2_CONFIG
    assert "CONFIG_NET_L2_ETHERNET=y" in TASK2_CONFIG


def test_board_timer_frequency_is_optional_and_recorded() -> None:
    assert 'timer_frequency_hz="${TASK2_TIMER_FREQUENCY_HZ:-}"' in BUILD_SCRIPT
    assert "CONFIG_SYS_CLOCK_HW_CYCLES_PER_SEC=%s" in BUILD_SCRIPT
    assert 'timer_frequency_hz = %s' in BUILD_SCRIPT


def test_runtime_trace_defaults_to_quiet_and_sampling_is_serialized() -> None:
    assert 'runtime_trace="${TASK2_RUNTIME_TRACE:-0}"' in BUILD_SCRIPT
    assert "-DTASK2_RUNTIME_TRACE=$runtime_trace" in BUILD_SCRIPT
    assert "src/console.c" in TASK2_CMAKE
    assert "src/telemetry.c" in TASK2_CMAKE
    assert "CONFIG_LOG=n" in TASK2_CONFIG
    assert "CONFIG_PRINTK_SYNC=y" in TASK2_CONFIG
    assert "task2_console_mutex" in CONSOLE_SOURCE
    assert "task2_console_set_trace_quiet" in CONSOLE_SOURCE
    assert "atomic_inc(&controls_received)" in TELEMETRY_SOURCE
