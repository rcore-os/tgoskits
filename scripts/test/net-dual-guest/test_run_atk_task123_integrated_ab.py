import importlib.util
import sys
from pathlib import Path


RUNNER = (
    Path(__file__).resolve().parents[2]
    / "board/run-atk-task123-integrated-ab.py"
).read_text()
RUNNER_PATH = Path(__file__).resolve().parents[2] / "board/run-atk-task123-integrated-ab.py"


def load_runner():
    spec = importlib.util.spec_from_file_location("task123_integrated_runner", RUNNER_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_runner_records_the_selected_rtos_without_hard_coding_rtthread() -> None:
    assert 'parser.add_argument("--rtos", choices=("rtthread", "zephyr")' in RUNNER
    assert "rtos_name=args.rtos" in RUNNER
    assert 'f"rtos={config.rtos_name}"' in RUNNER
    assert 'f"rtos={config.rtos_name} rtos_priority=90' in RUNNER
    assert '"rtthread_priority=90"' not in RUNNER


def test_runner_accepts_the_archived_manual_and_real_yolo_evidence() -> None:
    runner = load_runner()
    evidence = (
        Path(__file__).resolve().parents[3]
        / "results/atk-dlrk3588-task123-integrated-ab-20260824/logs"
    )
    for mode in ("manual", "yolo"):
        config = runner.RunConfig(
            mode=mode,
            rtos_name="rtthread",
            log_path=evidence / f"{mode}-console.log",
            metadata_path=evidence / f"{mode}-console.log.metadata.txt",
            port="unused",
            baud=1_500_000,
            artifacts=(),
        )
        runner.validate_log(config.log_path.read_bytes(), config)
