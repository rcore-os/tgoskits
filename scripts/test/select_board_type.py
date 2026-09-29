#!/usr/bin/env python3
"""Select the first registered board type from the supplied aliases."""

import argparse
import subprocess
import sys


def select_board_type(listing: str, candidates: list[str]) -> str | None:
    registered = {fields[0] for line in listing.splitlines() if (fields := line.split())}
    return next((candidate for candidate in candidates if candidate in registered), None)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidates", nargs="+", help="board types in preference order")
    args = parser.parse_args()

    result = subprocess.run(
        ["cargo", "xtask", "board", "ls"], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        print(result.stderr or result.stdout, file=sys.stderr, end="")
        return result.returncode

    selected = select_board_type(result.stdout, args.candidates)
    if selected is None:
        print(
            f"none of the requested board types is registered: {', '.join(args.candidates)}",
            file=sys.stderr,
        )
        print(result.stdout, file=sys.stderr, end="")
        return 1

    print(selected)
    return 0


if __name__ == "__main__":
    sys.exit(main())
