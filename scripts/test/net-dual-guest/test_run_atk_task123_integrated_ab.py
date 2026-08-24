from pathlib import Path


RUNNER = (
    Path(__file__).resolve().parents[2]
    / "board/run-atk-task123-integrated-ab.py"
).read_text()


def test_runner_records_the_selected_rtos_without_hard_coding_rtthread() -> None:
    assert 'parser.add_argument("--rtos", choices=("rtthread", "zephyr")' in RUNNER
    assert "rtos_name=args.rtos" in RUNNER
    assert 'f"rtos={config.rtos_name}"' in RUNNER
    assert 'f"rtos={config.rtos_name} rtos_priority=90' in RUNNER
    assert '"rtthread_priority=90"' not in RUNNER
