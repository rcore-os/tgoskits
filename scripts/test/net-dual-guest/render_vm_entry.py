#!/usr/bin/env python3
"""Render a VM config with the entry point from a freshly built manifest."""

from __future__ import annotations

import argparse
import re
import tomllib
from pathlib import Path


ENTRY_PATTERN = re.compile(r"(?m)^entry_point\s*=\s*0x[0-9a-fA-F_]+\s*$")


def render_vm_entry(manifest_path: Path, config_path: Path, output_path: Path) -> None:
    manifest_text = manifest_path.read_text()
    try:
        manifest = tomllib.loads(manifest_text)
    except tomllib.TOMLDecodeError:
        manifest = {}

    entry = manifest.get("elf_entry", manifest.get("entry_point"))
    if isinstance(entry, int):
        entry = hex(entry)
    if not isinstance(entry, str) or not re.fullmatch(r"0x[0-9a-fA-F]+", entry):
        match = re.search(
            r'(?m)^(?:elf_entry|entry_point)\s*=\s*"?(0x[0-9a-fA-F]+)"?\s*$',
            manifest_text,
        )
        entry = match.group(1) if match else None
    if not isinstance(entry, str):
        raise ValueError(f"manifest has no valid elf_entry: {manifest_path}")

    config = config_path.read_text()
    rendered, replacements = ENTRY_PATTERN.subn(f"entry_point = {entry}", config)
    if replacements != 1:
        raise ValueError(
            f"expected exactly one entry_point in {config_path}, found {replacements}"
        )
    output_path.write_text(rendered)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("config", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    render_vm_entry(args.manifest, args.config, args.output)


if __name__ == "__main__":
    main()
