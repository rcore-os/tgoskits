#!/usr/bin/env python3
"""Simple Python test running inside StarryOS."""

import sys
import os

def main():
    print(f"Hello from Python {sys.version} on StarryOS!")
    print(f"PID: {os.getpid()}")
    print(f"CWD: {os.getcwd()}")
    print(f"Platform: {sys.platform}")

    # Basic sanity checks
    assert 1 + 1 == 2, "math is broken"
    assert isinstance(os.getpid(), int), "getpid failed"
    assert len(os.getcwd()) > 0, "getcwd failed"

    print("TEST PASSED")

if __name__ == "__main__":
    main()
