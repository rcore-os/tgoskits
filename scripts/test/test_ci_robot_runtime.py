#!/usr/bin/env python3
"""Integration checks for the deployed single-guest robot applications.

These applications require host-owned MMC and private guest images. Check their
build/guest/smoke assembly here, separately from generic CI planning. Hardware
motion and storage I/O still require the board tests.
"""

import os
from pathlib import Path
import re
import selectors
import shlex
import subprocess
import tempfile
import time
import tomllib
import unittest


WORKSPACE_ROOT = Path(__file__).resolve().parents[2]
APPLICATION_ROOT = Path("apps/axvisor/normal/board-ballpick-robot")


def boot_option(cmdline: str, name: str) -> str | None:
    value = None
    for token in shlex.split(cmdline):
        if token == "--":
            break
        if token.startswith(name + "="):
            value = token.split("=", 1)[1]
    return value


class RobotRuntimeIntegrationTests(unittest.TestCase):
    def applications(self):
        for system in ("linux", "starry"):
            application = WORKSPACE_ROOT / APPLICATION_ROOT / f"robot-real-{system}"
            build = tomllib.loads(
                (application / "build-aarch64-unknown-none-softfloat.toml").read_text()
            )
            # Follow the host build's actual input rather than a second VM path list.
            self.assertEqual(len(build["vm_configs"]), 1, application)
            guest = tomllib.loads(
                (WORKSPACE_ROOT / build["vm_configs"][0]).read_text()
            )
            boards = list((application / "smoke").glob("board-*.toml"))
            self.assertTrue(boards, application)
            yield system, application, guest, [
                (path, tomllib.loads(path.read_text())) for path in boards
            ]

    def test_robot_guests_keep_host_mmc_and_mount_distinct_image_roots(self):
        images = []
        for system, application, guest, boards in self.applications():
            with self.subTest(system=system):
                # BallPick-Robot is the deployment compatibility boundary; the
                # native Starry suite uses a different physical board type.
                interactive = tomllib.loads(
                    (application / "configs/uboot-shell.toml").read_text()
                )
                root = boot_option(guest["kernel"]["cmdline"], "root")
                self.assertEqual(root, "/dev/vda" if system == "linux" else "/dev/sda")
                for board in [interactive, *(config for _, config in boards)]:
                    self.assertEqual(board["board_type"], "BallPick-Robot")
                    self.assertEqual(
                        boot_option(board["cmdline"], "axvisor.vm_start"),
                        str(guest["base"]["id"]),
                    )
                    self.assertNotEqual(root, boot_option(board["cmdline"], "root"))
                disks = [
                    device for device in guest["devices"]["virtual"]
                    if device["model"] == "virtio-blk"
                ]
                self.assertEqual(len(disks), 1)
                image = disks[0]["path"]
                self.assertTrue(Path(image).is_absolute(), image)
                self.assertFalse(image.startswith("/dev/"), image)
                self.assertEqual(disks[0]["filesystem"], "ext4")
                images.append(image)
                excluded = {device["path"] for device in guest["devices"]["disabled"]}
                self.assertTrue(
                    {"/mmc@fe2c0000", "/mmc@fe2e0000"} <= excluded,
                    "robot guests must leave both physical MMC controllers to the host",
                )
        self.assertEqual(len(images), len(set(images)), "guest root images must be private")

    def test_starry_preparation_builds_the_kernel_used_by_the_guest(self):
        for system, _, guest, boards in self.applications():
            if system != "starry":
                continue
            for path, _ in boards:
                with self.subTest(board=path):
                    sidecar = path.with_name("guest-builds.toml")
                    self.assertTrue(
                        sidecar.is_file(), "the automatic entry must prepare its guest kernel"
                    )
                    command = tomllib.loads(sidecar.read_text())["host_prepare"]
                    self.assertEqual(command[:4], ["cargo", "xtask", "starry", "build"])
                    config = WORKSPACE_ROOT / command[command.index("--config") + 1]
                    build = tomllib.loads(config.read_text())
                    self.assertIn("ax-driver/virtio-blk", build["features"])
                    self.assertEqual(
                        int(command[command.index("--smp") + 1]), guest["base"]["cpu_num"]
                    )
                    kernel = guest["kernel"]["kernel_path"].replace(
                        "${workspace}", str(WORKSPACE_ROOT)
                    )
                    self.assertEqual(
                        Path(kernel),
                        WORKSPACE_ROOT / "target" / build["target"] / "release/starryos.bin",
                        "the prepared kernel must be the payload the host packages",
                    )

    def test_robot_checks_require_launcher_success_and_completed_sync(self):
        for _, _, guest, boards in self.applications():
            for path, board in boards:
                with self.subTest(board=path), tempfile.TemporaryDirectory() as temporary:
                    self.check_robot_commands(guest, board, Path(temporary))

    def check_robot_commands(self, guest, board, directory):
        steps = board["shell_check_steps"]
        attach = [
            index for index, step in enumerate(steps)
            if step.get("shell_cmd", "").strip() == f"vm console {guest['base']['id']}"
        ]
        launch = [
            index for index, step in enumerate(steps)
            if "run_single_pick_ci_once.sh" in step.get("shell_cmd", "")
        ]
        self.assertEqual(len(attach), 1, "attach to the selected guest before launching")
        self.assertEqual(len(launch), 1, "run the single-pick launcher once")
        self.assertLess(attach[0], launch[0])
        hostname = boot_option(guest["kernel"]["cmdline"], "HOSTNAME")
        for step in steps[attach[0] + 1:]:
            if "shell_prefix" in step:
                self.assertEqual(step["shell_prefix"], f"root@{hostname}:~#")
        command = steps[launch[0]]["shell_cmd"]
        patterns = steps[launch[0]]["success_regex"]
        matches = lambda text: any(re.search(pattern, text) for pattern in patterns)
        for attempts in (1, 2):
            self.assertTrue(matches(f"[ROBOT_CI] RESULT=PASS attempts={attempts}\n"))
        for text in (
            command,
            "[ROBOT_CI] APPLICATION_PASS flow=1\n",
            "[ROBOT_CI] RESULT=FAIL attempts=2\n",
            "[ROBOT_CI] RESULT=PASS attempts=3\n",
            "[ROBOT_CI] RESULT=PASS attempts=1 garbage\n",
        ):
            self.assertFalse(matches(text), text)
        for producer in ("RESULT", "HARNESS"):
            self.assertTrue(any(
                re.search(pattern, f"[ROBOT_CI] {producer}=FAIL reason=test\n")
                for pattern in board["fail_regex"]
            ))

        completion = steps[-1]
        self.assertGreater(len(steps) - 1, launch[0])
        self.assertTrue(
            completion.get("success_regex"),
            "the last step must wait for the result after sync completes",
        )
        for text in (command, "[ROBOT_CI] RESULT=PASS attempts=1\n", "SYNC_WAIT\n"):
            self.assertFalse(any(
                re.search(pattern, text) for pattern in completion["success_regex"]
            ))

        # Only the external program and sync are substitutes. Execute
        # all configured guest commands in one shell, including the
        # actual deployment-directory assignment. Remap cd at the host
        # filesystem boundary; no robot binary can run on the host.
        script = directory / "run_single_pick_ci_once.sh"
        script.write_text(
            '#!/bin/sh\n[ "$FEETECH_DEV" = auto ] || exit 69\n'
            'printf "[ROBOT_CI] RESULT=PASS attempts=1\\n"\n'
            'exit "$ROBOT_TEST_EXIT"\n'
        )
        script.chmod(0o755)
        sync = directory / "sync"
        sync.write_text(
            '#!/bin/sh\nprintf "SYNC_WAIT\\n"\n'
            'read -r release || exit 69\n'
            '[ "$release" = continue ] || exit 69\n'
            'exit "$ROBOT_TEST_SYNC_EXIT"\n'
        )
        sync.chmod(0o755)
        commands = '\n'.join(
            step["shell_cmd"] for step in steps[attach[0] + 1:] if "shell_cmd" in step
        )
        remap_directory = '''
cd() {
    [ "$1" = /home/orangepi/robot/aka-rk3588-single ] || return 68
    command cd "$ROBOT_TEST_DIRECTORY"
}
'''
        for launcher_status, sync_status in ((0, 0), (7, 0), (0, 8)):
            environment = {
                **os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                # The configured command must override an inherited selector.
                "FEETECH_DEV": "must-be-set-by-config",
                "ROBOT_TEST_DIRECTORY": str(directory),
                "ROBOT_TEST_EXIT": str(launcher_status),
                "ROBOT_TEST_SYNC_EXIT": str(sync_status),
            }
            environment.pop("d", None)
            with subprocess.Popen(
                ["/bin/sh", "-c", remap_directory + commands],
                env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            ) as process:
                observed = b""
                try:
                    if launcher_status == 0:
                        deadline = time.monotonic() + 5
                        with selectors.DefaultSelector() as reader:
                            reader.register(process.stdout, selectors.EVENT_READ)
                            while b"SYNC_WAIT\n" not in observed:
                                self.assertTrue(
                                    reader.select(max(0, deadline - time.monotonic())),
                                    "the configured command did not reach sync",
                                )
                                chunk = os.read(process.stdout.fileno(), 4096)
                                self.assertTrue(chunk, observed)
                                observed += chunk
                        self.assertIsNone(process.poll(), "sync must wait for release")
                        self.assertFalse(any(
                            re.search(pattern, observed.decode())
                            for pattern in completion["success_regex"]
                        ), "the board must not pass while sync is blocked")
                    output, error = process.communicate(b"continue\n", timeout=5)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.communicate(timeout=5)
            output = (observed + output).decode()
            self.assertTrue(matches(output), output)
            passed = any(
                re.search(pattern, output) for pattern in completion["success_regex"]
            )
            failed = any(
                re.search(pattern, output)
                for pattern in board["fail_regex"] + completion.get("fail_regex", [])
            )
            expected = launcher_status == 0 and sync_status == 0
            self.assertEqual(passed, expected, (output, error))
            self.assertEqual(failed, not expected, (output, error))
            self.assertEqual("SYNC_WAIT" in output, launcher_status == 0, output)


if __name__ == "__main__":
    unittest.main()
