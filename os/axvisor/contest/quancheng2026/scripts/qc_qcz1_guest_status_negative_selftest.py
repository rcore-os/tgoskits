#!/usr/bin/env python3

import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def repo_root() -> Path:
    return Path(__file__).resolve().parents[5]


def compile_harness(work_dir: Path, source: Path) -> Path:
    harness = work_dir / "qc_qcz1_guest_status_selftest.c"
    binary = work_dir / "qc_qcz1_guest_status_selftest"
    harness.write_text(
        f'''
#define QCZ1_HOST_SELFTEST 1
#include "{source.as_posix()}"

int main(int argc, char **argv) {{
    if (argc != 2) {{
        return 100;
    }}
    if (strcmp(argv[1], "ok") == 0) {{
        qcz1_selftest_status_mode = QCZ1_SELFTEST_STATUS_OK;
    }} else if (strcmp(argv[1], "status-timeout") == 0) {{
        qcz1_selftest_status_mode = QCZ1_SELFTEST_STATUS_TIMEOUT;
    }} else if (strcmp(argv[1], "status-malformed") == 0) {{
        qcz1_selftest_status_mode = QCZ1_SELFTEST_STATUS_MALFORMED;
    }} else {{
        return 101;
    }}

    return run_demo();
}}
'''.lstrip(),
        encoding="utf-8",
    )
    cc = os.environ.get("CC") or shutil.which("cc") or shutil.which("gcc") or shutil.which("clang")
    if not cc:
        raise RuntimeError("no C compiler found; set CC or install cc/gcc/clang")
    subprocess.run(
        [
            cc,
            "-std=c99",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-O2",
            str(harness),
            "-o",
            str(binary),
        ],
        check=True,
    )
    return binary


def run_case(binary: Path, mode: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(binary), mode],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )


def require(condition: bool, message: str, output: str = "") -> None:
    if condition:
        return
    print(f"QC_QCZ1_STATUS_NEGATIVE_SELFTEST_FAIL={message}")
    if output:
        print("--- output ---")
        print(output)
    raise SystemExit(1)


def main() -> int:
    root = repo_root()
    source = root / "os/axvisor/contest/quancheng2026/linux/qc_qcz1_guest_demo.c"
    with tempfile.TemporaryDirectory(prefix="qc-qcz1-status-selftest-") as tmp:
        binary = compile_harness(Path(tmp), source)

        ok = run_case(binary, "ok")
        require(ok.returncode == 0, "ok_case_nonzero", ok.stdout)
        require("QC_QCZ1_RELIABLE_STATUS_OK=1" in ok.stdout, "ok_missing_reliable_status", ok.stdout)
        require("QC_AI_STATUS_OK=1" in ok.stdout, "ok_missing_ai_status", ok.stdout)
        require("QC_QCZ1_GUEST_DEMO=PASS" in ok.stdout, "ok_missing_pass", ok.stdout)

        timeout = run_case(binary, "status-timeout")
        require(timeout.returncode != 0, "timeout_case_zero", timeout.stdout)
        require("QC_QCZ1_STATUS_RESULT=IO_ERROR" in timeout.stdout, "timeout_missing_io_error", timeout.stdout)
        require("QC_QCZ1_RELIABLE_STATUS_OK=0" in timeout.stdout, "timeout_missing_status_zero", timeout.stdout)
        require("QC_QCZ1_GUEST_DEMO=FAIL" in timeout.stdout, "timeout_missing_fail", timeout.stdout)
        require("QC_QCZ1_GUEST_DEMO=PASS" not in timeout.stdout, "timeout_unexpected_pass", timeout.stdout)

        malformed = run_case(binary, "status-malformed")
        require(malformed.returncode != 0, "malformed_case_zero", malformed.stdout)
        require("QC_QCZ1_STATUS_RESULT=BAD_FRAME" in malformed.stdout, "malformed_missing_bad_frame", malformed.stdout)
        require("QC_QCZ1_RELIABLE_STATUS_OK=0" in malformed.stdout, "malformed_missing_status_zero", malformed.stdout)
        require("QC_QCZ1_GUEST_DEMO=FAIL" in malformed.stdout, "malformed_missing_fail", malformed.stdout)
        require("QC_QCZ1_GUEST_DEMO=PASS" not in malformed.stdout, "malformed_unexpected_pass", malformed.stdout)

    print("QC_QCZ1_STATUS_NEGATIVE_SELFTEST=PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
