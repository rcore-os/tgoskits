#!/usr/bin/env python3

import tempfile
import unittest
from pathlib import Path

import publish_preflight


class PublishPreflightTests(unittest.TestCase):
    def test_only_crates_io_publishable_workspace_members_become_patches(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            metadata = {
                "workspace_members": ["path+file:///a#a@1.0.0", "custom@1.0.0"],
                "packages": [
                    {
                        "id": "path+file:///a#a@1.0.0",
                        "name": "a",
                        "manifest_path": str(root / "a" / "Cargo.toml"),
                        "publish": None,
                    },
                    {
                        "id": "custom@1.0.0",
                        "name": "custom",
                        "manifest_path": str(root / "custom" / "Cargo.toml"),
                        "publish": ["private"],
                    },
                    {
                        "id": "outside@1.0.0",
                        "name": "outside",
                        "manifest_path": str(root / "outside" / "Cargo.toml"),
                        "publish": None,
                    },
                ],
            }

            self.assertEqual(
                publish_preflight.workspace_patch_paths(metadata),
                {"a": (root / "a").resolve()},
            )

    def test_rendered_patch_config_is_stable_and_uses_absolute_paths(self) -> None:
        config = publish_preflight.render_patch_config(
            {
                "z-crate": Path("/workspace/z"),
                "a-crate": Path("/workspace/a"),
            }
        )

        self.assertEqual(
            config,
            "[patch.crates-io]\n"
            '"a-crate" = { path = "/workspace/a" }\n'
            '"z-crate" = { path = "/workspace/z" }\n',
        )

    def test_publish_command_keeps_the_registry_dry_run_contract(self) -> None:
        config = Path("/tmp/workspace-patches.toml")

        self.assertEqual(
            publish_preflight.publish_command(
                config, allow_dirty=False, exclude={"cycle-b", "cycle-a"}
            ),
            [
                "cargo",
                "publish",
                "--dry-run",
                "--no-verify",
                "--quiet",
                "--config",
                str(config),
                "--workspace",
                "--exclude",
                "cycle-a",
                "--exclude",
                "cycle-b",
            ],
        )
        self.assertEqual(
            publish_preflight.publish_command(
                config, allow_dirty=True, package="ax-net"
            )[-3:],
            ["--package", "ax-net", "--allow-dirty"],
        )

    def test_dev_dependency_cycles_are_split_from_the_workspace_batch(self) -> None:
        graph = {
            "leaf": set(),
            "cycle-a": {"cycle-b"},
            "cycle-b": {"cycle-a"},
            "self-cycle": {"self-cycle"},
        }

        self.assertEqual(
            publish_preflight.cyclic_packages(graph),
            {"cycle-a", "cycle-b", "self-cycle"},
        )

    def test_path_only_dev_dependency_is_not_a_publish_order_edge(self) -> None:
        metadata = {
            "workspace_members": ["a", "b"],
            "packages": [
                {
                    "id": "a",
                    "name": "a",
                    "dependencies": [
                        {"name": "b", "kind": "dev", "req": "*"},
                    ],
                },
                {
                    "id": "b",
                    "name": "b",
                    "dependencies": [
                        {"name": "a", "kind": None, "req": "^1"},
                    ],
                },
            ],
        }

        self.assertEqual(
            publish_preflight.publish_dependency_graph(
                metadata, {"a": Path("/a"), "b": Path("/b")}
            ),
            {"a": set(), "b": {"a"}},
        )

    def test_allow_dirty_is_last_for_workspace_command(self) -> None:
        config = Path("/tmp/workspace-patches.toml")

        self.assertEqual(
            publish_preflight.publish_command(config, allow_dirty=True)[-1],
            "--allow-dirty",
        )


if __name__ == "__main__":
    unittest.main()
