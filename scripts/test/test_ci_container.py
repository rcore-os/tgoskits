#!/usr/bin/env python3

import os
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path

from check_ci_routing import named_step_block

WORKSPACE_ROOT = Path(__file__).resolve().parents[2]


class ContainerPublishTests(unittest.TestCase):
    def resolve_targets(self, **inputs: str) -> dict[str, str]:
        workflow = (
            WORKSPACE_ROOT / ".github/workflows/container-publish.yml"
        ).read_text(encoding="utf-8")
        step = named_step_block(workflow, "Resolve publish targets")
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "output"
            environment = {
                **os.environ,
                "EVENT_NAME": "push",
                "REF_NAME": "dev",
                "MANUAL_TARGET": "both",
                "FILTER_BASE": "false",
                "FILTER_AXVISOR": "false",
                "GITHUB_REPOSITORY": "Example/Project",
                "GITHUB_OUTPUT": str(output),
                **inputs,
            }
            subprocess.run(
                ["bash", "-euo", "pipefail", "-c", script],
                env=environment,
                check=True,
                capture_output=True,
                text=True,
            )
            return dict(line.split("=", 1) for line in output.read_text().splitlines())

    def test_changed_base_rebuilds_its_derived_image(self) -> None:
        for base, derived in (("false", "false"), ("false", "true"), ("true", "false")):
            with self.subTest(base=base, derived=derived):
                targets = self.resolve_targets(FILTER_BASE=base, FILTER_AXVISOR=derived)
                self.assertEqual(targets["base"], base)
                self.assertEqual(targets["axvisor"], "true" if "true" in (base, derived) else "false")
                self.assertEqual(targets["any"], targets["axvisor"])

    def test_manual_selection_preserves_explicit_targets_and_branch_gate(self) -> None:
        for target, expected in (
            ("base", ("true", "false")),
            ("axvisor-lvz", ("false", "true")),
            ("both", ("true", "true")),
        ):
            with self.subTest(target=target):
                targets = self.resolve_targets(EVENT_NAME="workflow_dispatch", MANUAL_TARGET=target)
                self.assertEqual((targets["base"], targets["axvisor"]), expected)
        targets = self.resolve_targets(EVENT_NAME="workflow_dispatch", REF_NAME="feature")
        self.assertEqual(targets["any"], "false")


if __name__ == "__main__":
    unittest.main()
