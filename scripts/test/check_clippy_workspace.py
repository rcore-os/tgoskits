#!/usr/bin/env python3
"""Exercise workspace discovery, new-member scheduling and lint failure via the CLI."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def check(tool):
    environment = dict(os.environ)
    environment["RUSTUP_TOOLCHAIN"] = subprocess.check_output(
        ["rustup", "show", "active-toolchain"], text=True
    ).split()[0]
    environment["CARGO_NET_OFFLINE"] = "true"
    with tempfile.TemporaryDirectory(prefix="clippy-workspace-") as directory:
        root = Path(directory)
        (root / "Cargo.toml").write_text('[workspace]\nmembers=["members/*"]\nresolver="3"\n')

        def package(path, name):
            (path / "src").mkdir(parents=True)
            (path / "Cargo.toml").write_text(
                f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2024"\n[features]\ncapability=[]\n'
            )
            (path / "src/lib.rs").write_text('pub fn identity(x: u64) -> u64 { x }\n')

        def invoke(*arguments, success=True):
            result = subprocess.run([str(tool), "clippy", *map(str, arguments)],
                                    cwd=root, env=environment, capture_output=True, text=True)
            if (result.returncode == 0) != success:
                raise RuntimeError(f"unexpected Clippy result {result.returncode}: {arguments}\n{result.stdout}\n{result.stderr}")
            return result.stdout + result.stderr

        package(root / "members/original", "fixture-original")
        package(root / "standalone", "fixture-standalone")
        with (root / "standalone/Cargo.toml").open("a") as stream:
            stream.write('\n[workspace]\n')
        plan = root / "target/plan.json"
        invoke("--all", "--emit-plan", plan, "--shards", "4")
        before = json.loads(plan.read_text())
        if {row["package"] for row in before["checks"]} != {"fixture-original"}:
            raise AssertionError("workspace discovery selected a non-member")
        new = root / "members/new"
        package(new, "fixture-new")
        # The workspace manifest is unchanged: its existing wildcard discovers this package.
        invoke("--all", "--emit-plan", plan, "--shards", "4")
        after = json.loads(plan.read_text())
        if {row["package"] for row in after["checks"]} != {"fixture-original", "fixture-new"}:
            raise AssertionError("new workspace package was omitted")
        if len(after["checks"]) != len(before["checks"]) + 2:
            raise AssertionError("new package feature matrix was not expanded")
        if sorted(index for shard in after["shards"] for index in shard) != list(range(len(after["checks"]))):
            raise AssertionError("new member was not assigned exactly once")
        results = root / "target/results"
        for shard in range(1, len(after["shards"]) + 1):
            invoke("--execute-plan", plan, "--shard", shard, "--result-file", results / f"{shard}.json")
        invoke("--verify-results", plan, "--results-dir", results)
        (new / "src/lib.rs").write_text(
            '#![deny(clippy::unwrap_used)]\n#[cfg(feature="capability")]\npub fn required(v: Option<u64>) -> u64 { v.unwrap() }\n'
        )
        feature = next(index for index, row in enumerate(after["checks"])
                       if row["package"] == "fixture-new" and row["kind"] == {"Feature": "capability"})
        shard = next(index + 1 for index, members in enumerate(after["shards"]) if feature in members)
        failure = invoke("--execute-plan", plan, "--shard", shard,
                         "--result-file", results / f"{shard}.json", success=False)
        if "clippy::unwrap_used" not in failure:
            raise AssertionError("expected genuine lint diagnostic was not observed")
        invoke("--verify-results", plan, "--results-dir", results, success=False)
        obsolete = invoke("--since", "HEAD", success=False)
        if "unexpected argument" not in obsolete:
            raise AssertionError("removed incremental selector still accepted")
    print("CLIPPY_WORKSPACE_CONTRACT_PASS: new member and feature discovered, scheduled, executed; lint failure rejected")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tool", required=True, type=Path)
    args = parser.parse_args()
    check(args.tool.resolve())


if __name__ == "__main__":
    main()
