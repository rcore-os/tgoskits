#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Capture one reusable, post-run Zephyr source provenance attestation."""

from __future__ import annotations

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Sequence

from analyze import AnalysisError, artifact, git_index_path, source_metadata


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--zephyr-base", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    """Capture source identity once after all measured QEMU cases exit."""
    args = parse_args(sys.argv[1:] if argv is None else argv)
    temporary_output = args.output.with_name(f".{args.output.name}.{os.getpid()}.tmp")
    try:
        if args.output.exists():
            raise AnalysisError(f"refusing to overwrite evidence: {args.output}")
        source = source_metadata(args.zephyr_base)
        git_index = artifact(git_index_path(args.zephyr_base), ".git/index")
        result = {
            "schema_version": 1,
            "captured_at_utc": datetime.now(timezone.utc).isoformat(),
            "zephyr_base": str(args.zephyr_base.resolve()),
            "source": source,
            "git_index": git_index,
            "validity": (
                "post-run bounded attestation; analyzers recheck identity and index"
            ),
        }
        rendered = json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n"
        temporary_output.write_text(rendered, encoding="utf-8")
        os.link(temporary_output, args.output)
    except (AnalysisError, OSError, UnicodeError) as error:
        print(f"source provenance capture failed: {error}", file=sys.stderr)
        return 2
    finally:
        temporary_output.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
