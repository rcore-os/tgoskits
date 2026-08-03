#!/usr/bin/env python3

import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
GUEST_RESTART_WORKER = REPOSITORY_ROOT / "os/axvisor/src/guest_restart.rs"


class AxvisorGuestRestartContractTests(unittest.TestCase):
    def test_post_start_delay_does_not_depend_on_sleep_timer_wakeup(self) -> None:
        source = GUEST_RESTART_WORKER.read_text(encoding="utf-8")

        self.assertIn("fn wait_cooperatively", source)
        self.assertIn("thread::yield_now();", source)
        self.assertNotIn("thread::sleep(", source)


if __name__ == "__main__":
    unittest.main()
