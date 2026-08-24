from pathlib import Path


ROOT = Path(__file__).resolve().parent
BUILD_SCRIPT = (ROOT / "build-zephyr-task2.sh").read_text()
BOARD_OVERLAY = (
    ROOT / "zephyr-task2/atk-dlrk3588-axvisor.overlay"
)
TASK2_CMAKE = (ROOT / "zephyr-task2/CMakeLists.txt").read_text()
TASK2_CONFIG = (ROOT / "zephyr-task2/prj.conf").read_text()
PERIODIC_SOURCE = ROOT / "zephyr-task2/src/periodic.c"


def test_task2_build_accepts_a_physical_guest_overlay() -> None:
    assert 'extra_overlay="${TASK2_ZEPHYR_EXTRA_OVERLAY:-}"' in BUILD_SCRIPT
    assert 'overlay_files=("$device_overlay" "$memory_overlay")' in BUILD_SCRIPT
    assert 'overlay_files+=("$(realpath "$extra_overlay")")' in BUILD_SCRIPT
    assert 'printf \'extra_overlay = "%s"\\n\'' in BUILD_SCRIPT


def test_standalone_zephyr_checkout_gets_an_empty_module_environment() -> None:
    assert 'mkdir -p "$build_dir/Kconfig"' in BUILD_SCRIPT
    assert 'set(kconfig_env_dirs)' in BUILD_SCRIPT


def test_task2_slot_metadata_matches_the_selected_virtio_window() -> None:
    assert 'fdt_path="/virtio_mmio@a000000"' in BUILD_SCRIPT
    assert "guest_irq=48" in BUILD_SCRIPT
    assert 'fdt_path="/virtio_mmio@a003c00"' in BUILD_SCRIPT
    assert "guest_irq=78" in BUILD_SCRIPT
    assert 'printf \'guest_irq = %s\\n\' "$guest_irq"' in BUILD_SCRIPT


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
