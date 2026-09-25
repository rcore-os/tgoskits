#!/usr/bin/env python3
"""Retry the resume866 diagnostic image with board-facing file transfer."""

import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PREVIOUS = ROOT.parent / "resume866-user-return"
spec = importlib.util.spec_from_file_location("resume866_board", PREVIOUS / "board.py")
board = importlib.util.module_from_spec(spec)
spec.loader.exec_module(board)

board.ROOT = ROOT
board.RUN = ROOT / "run1"
board.IMAGE = PREVIOUS / "image.bin"
board.API = "http://192.168.1.2:2999"
board.helper.API = "http://10.3.10.194:2999"


def check_source():
    assert board.helper.sha(board.IMAGE) == (
        "54ee6a87398275a571f917ac55526947ea5eb2f440c75e6aad55fbf8af39537f"
    )
    assert board.helper.sha(PREVIOUS / "probe-source.tgz") == (
        "74851e4473cc54f3754256dac82b95a63ffeb96350f351b69dd0222d4ca5f755"
    )
    assert board.helper.sha(board.BENCH) == board.BENCH_SHA


board.check_source = check_source

if __name__ == "__main__":
    board.main()
