import importlib.util
from pathlib import Path

import pytest


SCRIPT = Path(__file__).with_name("analyze-hybrid-latency.py")
SPEC = importlib.util.spec_from_file_location("analyze_hybrid_latency", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
ANALYZER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ANALYZER)


def make_capture(*, idle: bool = False, inside_window: str = "") -> str:
    activity = "controls=0 statuses=0 heartbeats=0" if idle else (
        "controls=3 statuses=3 heartbeats=2"
    )
    rows = []
    for sequence, jitter in enumerate((100_000, -20_000, 3_500_000)):
        deadline = 100_000_000 + sequence * 10_000_000
        actual = deadline + jitter
        timestamp = (sequence + 1) * 10_000_000 + jitter
        rows.append(f"{sequence},{timestamp},{deadline},{actual},{jitter}\n")
    return (
        "PERIODIC LATENCY READY frequency_hz=24000000 period_ms=10 samples=3\n"
        "PERIODIC LATENCY START\n"
        f"{inside_window}"
        "PERIODIC LATENCY SAMPLING COMPLETE samples=3 "
        f"{activity}\n"
        "sequence,timestamp_ns,deadline_ns,actual_ns,jitter_ns\n"
        + "".join(rows)
        + "PERIODIC LATENCY COMPLETE samples=3\n"
    )


def test_parses_strict_stress_capture_and_preserves_all_rows(tmp_path: Path) -> None:
    rows, activity = ANALYZER.parse_capture(make_capture(), 3, False)

    assert activity == (3, 3, 2)
    assert [row[4] for row in rows] == [100_000, -20_000, 3_500_000]

    output = tmp_path / "analysis"
    ANALYZER.write_analysis(output, rows, activity, 24_000_000)
    assert "deadline_misses=0\n" in (output / "summary.txt").read_text()
    assert len((output / "spikes-over-3ms.csv").read_text().splitlines()) == 2


def test_accepts_idle_only_when_control_and_status_counts_are_zero() -> None:
    rows, activity = ANALYZER.parse_capture(make_capture(idle=True), 3, True)

    assert len(rows) == 3
    assert activity == (0, 0, 0)


def test_rejects_uart_output_inside_sampling_window() -> None:
    with pytest.raises(ValueError, match="UART output occurred"):
        ANALYZER.parse_capture(make_capture(inside_window="TASK2_TRACE\n"), 3, False)


def test_rejects_capture_with_the_wrong_board_timer_frequency() -> None:
    capture = make_capture().replace("frequency_hz=24000000", "frequency_hz=62500000")

    with pytest.raises(ValueError):
        ANALYZER.parse_capture(capture, 3, False)


def test_rejects_interleaved_csv_line() -> None:
    capture = make_capture().replace(
        "1,19980000,110000000,109980000,-20000",
        "1,19980000,TASK2_ACK,110000000,109980000,-20000",
    )

    with pytest.raises(ValueError, match="invalid CSV line"):
        ANALYZER.parse_capture(capture, 3, False)
