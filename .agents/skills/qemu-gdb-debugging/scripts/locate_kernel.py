#!/usr/bin/env python3
"""Calculate a kernel load bias and emit GDB symbol commands.

The script is deliberately read-only: it invokes binutils on the supplied ELF
and writes an optional GDB command file, but never changes the ELF or target.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


LOAD_RE = re.compile(
    r"^\s*LOAD\s+0x([0-9a-fA-F]+)\s+0x([0-9a-fA-F]+)\s+"
    r"0x([0-9a-fA-F]+)\s+0x([0-9a-fA-F]+)\s+0x([0-9a-fA-F]+)\s+"
    r"([RWE ]+)\s+0x([0-9a-fA-F]+)"
)
SECTION_RE = re.compile(
    r"^\s*\[\s*\d+\]\s+(\S+)\s+\S+\s+"
    r"([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+([0-9a-fA-F]+)\s+"
    r"\S+\s+([A-Z]+)"
)
SYMBOL_RE = re.compile(r"^([0-9a-fA-F]+)\s+\S\s+(.+)$")


def run(*args: str) -> str:
    result = subprocess.run(args, check=True, text=True, capture_output=True)
    return result.stdout


def parse_loads(elf: Path) -> list[dict[str, int | str]]:
    loads = []
    for line in run("readelf", "-W", "-l", str(elf)).splitlines():
        match = LOAD_RE.match(line)
        if match:
            offset, virt, phys, filesz, memsz, flags, align = match.groups()
            loads.append(
                {
                    "offset": int(offset, 16),
                    "virt": int(virt, 16),
                    "phys": int(phys, 16),
                    "filesz": int(filesz, 16),
                    "memsz": int(memsz, 16),
                    "flags": flags.strip(),
                    "align": int(align, 16),
                }
            )
    return loads


def parse_sections(elf: Path) -> dict[str, tuple[int, str]]:
    sections: dict[str, tuple[int, str]] = {}
    for line in run("readelf", "-W", "-S", str(elf)).splitlines():
        match = SECTION_RE.match(line)
        if match:
            name, address, _offset, _size, flags = match.groups()
            sections[name] = (int(address, 16), flags)
    return sections


def find_symbol(elf: Path, needle: str) -> tuple[str, int]:
    exact: tuple[str, int] | None = None
    candidates: list[tuple[str, int]] = []
    for line in run("nm", "-C", "--defined-only", str(elf)).splitlines():
        match = SYMBOL_RE.match(line)
        if not match:
            continue
        address, name = match.groups()
        value = int(address, 16)
        if name == needle:
            exact = (name, value)
            break
        if needle in name:
            candidates.append((name, value))
    if exact:
        return exact
    if len(candidates) == 1:
        return candidates[0]
    if not candidates:
        raise ValueError(f"symbol not found: {needle}")
    names = ", ".join(name for name, _ in candidates[:5])
    raise ValueError(f"symbol is ambiguous ({names}); use a full demangled name")


def parse_int(value: str) -> int:
    return int(value, 0)


def format_hex(value: int) -> str:
    return f"-0x{-value:x}" if value < 0 else f"0x{value:x}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--elf", required=True, type=Path)
    parser.add_argument("--runtime-pc", required=True, type=parse_int)
    parser.add_argument("--symbol", help="demangled symbol whose runtime PC was observed")
    parser.add_argument("--link-pc", type=parse_int, help="link-time PC when --symbol is omitted")
    parser.add_argument("--section", action="append", help="extra section to print")
    parser.add_argument("--gdb-script-out", type=Path)
    args = parser.parse_args()

    if not args.elf.is_file():
        parser.error(f"ELF does not exist: {args.elf}")
    args.elf = args.elf.resolve()
    if args.symbol and args.link_pc is not None:
        parser.error("use only one of --symbol and --link-pc")
    try:
        loads = parse_loads(args.elf)
        sections = parse_sections(args.elf)
        if not loads:
            raise ValueError("ELF has no PT_LOAD segment")
        if args.symbol:
            symbol_name, link_pc = find_symbol(args.elf, args.symbol)
        elif args.link_pc is not None:
            symbol_name, link_pc = "<link-pc>", args.link_pc
        else:
            raise ValueError("one of --symbol or --link-pc is required")
    except (OSError, subprocess.CalledProcessError, ValueError) as error:
        print(f"locate_kernel.py: {error}", file=sys.stderr)
        return 2

    load_bias = args.runtime_pc - link_pc
    executable = [segment for segment in loads if "E" in str(segment["flags"])]
    text_section_name = ".text" if ".text" in sections else ".head.text"
    text_address = sections.get(text_section_name)
    if text_address is None and executable:
        text_address = (int(executable[0]["virt"]), "")
    if text_address is None:
        text_address = (int(loads[0]["virt"]), "")
    gdb_text = text_address[0] + load_bias
    add_symbol = [f"add-symbol-file {args.elf} {format_hex(gdb_text)}"]
    try:
        for section in args.section or []:
            if section not in sections:
                raise ValueError(f"section not found: {section}")
            if section == text_section_name:
                continue
            address = sections[section][0] + load_bias
            add_symbol.extend(("-s", section, format_hex(address)))
    except ValueError as error:
        print(f"locate_kernel.py: {error}", file=sys.stderr)
        return 2
    commands = [" ".join(add_symbol), f"set $kernel_load_bias = {format_hex(load_bias)}"]
    commands.append(f"break *{format_hex(args.runtime_pc)}")

    print(f"elf={args.elf}")
    print(f"symbol={symbol_name}")
    print(f"link_pc={format_hex(link_pc)}")
    print(f"runtime_pc={format_hex(args.runtime_pc)}")
    print(f"load_bias={format_hex(load_bias)}")
    print("gdb_commands:")
    print("\n".join(commands))
    if args.gdb_script_out:
        try:
            args.gdb_script_out.write_text("\n".join(commands) + "\n", encoding="utf-8")
        except OSError as error:
            print(f"locate_kernel.py: cannot write GDB script: {error}", file=sys.stderr)
            return 2
        print(f"gdb_script={args.gdb_script_out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
