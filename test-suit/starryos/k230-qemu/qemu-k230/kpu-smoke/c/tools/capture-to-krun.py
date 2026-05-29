#!/usr/bin/env python3
import argparse
import json
import sys

COMMANDS_PER_LINE = 256
BYTES_PER_LINE = 256


def parse_int(value):
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        return int(value, 0)
    raise TypeError(f"expected integer-like value, got {value!r}")


def hex_u32(value):
    parsed = parse_int(value)
    if parsed < 0 or parsed > 0xFFFFFFFF:
        raise ValueError(f"command word out of range: {value!r}")
    return f"0x{parsed:08x}"


def hex_u8(value):
    parsed = parse_int(value)
    if parsed < 0 or parsed > 0xFF:
        raise ValueError(f"byte out of range: {value!r}")
    return f"0x{parsed:02x}"


def hex_u64(value):
    parsed = parse_int(value)
    if parsed < 0 or parsed > 0xFFFFFFFFFFFFFFFF:
        raise ValueError(f"64-bit value out of range: {value!r}")
    return f"0x{parsed:016x}"


def emit_bytes(values):
    return " ".join(hex_u8(value) for value in values)


def parse_hex_bytes(text):
    compact = "".join(text.split())
    if compact.startswith("0x") or compact.startswith("0X"):
        compact = compact[2:]
    if len(compact) % 2 != 0:
        raise ValueError("hex byte string must contain an even number of digits")
    return [int(compact[i : i + 2], 16) for i in range(0, len(compact), 2)]


def get_byte_values(source, list_key, hex_key):
    if list_key in source:
        return [parse_int(value) for value in source[list_key]]
    if hex_key in source:
        return parse_hex_bytes(source[hex_key])
    raise ValueError(f"capture entry must contain {list_key!r} or {hex_key!r}")


def chunks(values, chunk_size):
    for start in range(0, len(values), chunk_size):
        yield start, values[start : start + chunk_size]


def emit_check(lines, check, op):
    window = check["window"]
    offset = parse_int(check["offset"])
    total_len = parse_int(check["total_len"])
    if "fnv1a64" in check:
        lines.append(f"{op}_hash {window} 0x{offset:x} {total_len} {hex_u64(check['fnv1a64'])}")
    else:
        tail = hex_u8(check.get("tail", 0))
        expected = get_byte_values(check, "expected", "expected_hex")
        lines.append(f"{op} {window} 0x{offset:x} {total_len} {tail} {emit_bytes(expected)}")


def emit_section(lines, section, prefix=""):
    kind = section["kind"]
    window = section["window"]
    offset = parse_int(section["offset"])
    if kind == "copy":
        data = get_byte_values(section, "data", "data_hex")
        for start, data_chunk in chunks(data, BYTES_PER_LINE):
            lines.append(f"{prefix}copy {window} 0x{offset + start:x} {emit_bytes(data_chunk)}")
    elif kind == "copy_file":
        file_path = section["path"]
        file_offset = parse_int(section.get("file_offset", 0))
        length = parse_int(section["len"])
        lines.append(
            f"{prefix}copy_file {window} 0x{offset:x} {file_path} 0x{file_offset:x} {length}"
        )
    elif kind == "fill":
        lines.append(
            f"{prefix}fill {window} 0x{offset:x} {parse_int(section['len'])} "
            f"{hex_u8(section['byte'])}"
        )
    else:
        raise ValueError(f"unsupported section kind: {kind}")


def emit_krun(capture):
    lines = ["kpu-runtime-image-v1"]
    lines.append(f"name {capture.get('name', 'captured_runtime')}")

    runs = capture.get("runs", [])
    command_file = capture.get("command_file")
    commands = capture.get("commands", [])
    if runs:
        if command_file is not None or commands:
            raise ValueError("capture must not contain both runs and legacy command data")
        for run in runs:
            run_command_file = run["command_file"]
            lines.append(
                f"run_file 0x{parse_int(run['command_paddr']):x} "
                f"{run_command_file['path']} "
                f"0x{parse_int(run_command_file.get('file_offset', 0)):x} "
                f"{parse_int(run_command_file['len'])}"
            )
            for section in run.get("sections", []):
                emit_section(lines, section, "run_")
            for check in run.get("checks", []):
                emit_check(lines, check, "run_check")
    else:
        lines.append(f"command_paddr 0x{parse_int(capture['command_paddr']):x}")
        if command_file is not None:
            if commands:
                raise ValueError("capture must not contain both command_file and commands")
            lines.append(
                f"command_file {command_file['path']} "
                f"0x{parse_int(command_file.get('file_offset', 0)):x} {parse_int(command_file['len'])}"
            )
        else:
            if not commands:
                raise ValueError("capture must contain command_file or at least one command word")
            for _, command_chunk in chunks(commands, COMMANDS_PER_LINE):
                lines.append("commands " + " ".join(hex_u32(command) for command in command_chunk))

    for section in capture.get("sections", []):
        emit_section(lines, section)

    for check in capture.get("checks", []):
        emit_check(lines, check, "check")

    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(
        description="Convert a captured K230 KPU runtime JSON file into smoke-test .krun text."
    )
    parser.add_argument("capture_json", help="input JSON capture")
    parser.add_argument("-o", "--output", help="output .krun path; defaults to stdout")
    args = parser.parse_args()

    with open(args.capture_json, "r", encoding="utf-8") as file:
        capture = json.load(file)
    krun = emit_krun(capture)

    if args.output:
        with open(args.output, "w", encoding="utf-8") as file:
            file.write(krun)
    else:
        sys.stdout.write(krun)


if __name__ == "__main__":
    main()
