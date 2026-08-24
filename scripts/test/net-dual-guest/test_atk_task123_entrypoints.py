from pathlib import Path
import subprocess


REPO_ROOT = Path(__file__).resolve().parents[3]
BUILD = REPO_ROOT / "scripts/board/build-atk-zephyr-task123-unified.sh"
SELECT = REPO_ROOT / "scripts/board/select-atk-task123-rtos.sh"


def run(*arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(arguments, text=True, capture_output=True, check=False)


def test_unified_builder_is_syntax_valid_and_builds_both_schedulers() -> None:
    assert run("bash", "-n", str(BUILD)).returncode == 0
    source = BUILD.read_text()
    assert "build_scheduler_variant rr rr-scheduler RR" in source
    assert "build_scheduler_variant fp-rr fp-rr-scheduler FP-RR" in source
    assert source.count("build_unified_zephyr_guest") == 2
    assert "fastboot stage" not in source
    assert "fastboot flash" not in source
    assert "fastboot erase" not in source


def test_selector_resolves_frozen_zephyr_rr_and_fp_rr() -> None:
    for scheduler, expected_name in (
        ("rr", "axvisor-task123-zephyr-rr.fit"),
        ("fp-rr", "axvisor-task123-zephyr-fp-rr.fit"),
    ):
        result = run(str(SELECT), "zephyr", scheduler)
        assert result.returncode == 0, result.stderr
        assert f"scheduler={scheduler}" in result.stdout
        assert expected_name in result.stdout
        assert "sha256=" in result.stdout


def test_selector_keeps_rtthread_and_rejects_unbuilt_full_rr_arm() -> None:
    result = run(str(SELECT), "rtthread", "fp-rr")
    assert result.returncode == 0, result.stderr
    assert "axvisor-task123-integrated-fp-rr.fit" in result.stdout

    unsupported = run(str(SELECT), "rtthread", "rr")
    assert unsupported.returncode == 2
    assert "no frozen full Task 1/2/3 RT-Thread RR FIT" in unsupported.stderr
