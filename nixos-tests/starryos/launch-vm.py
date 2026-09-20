#!/usr/bin/env python3

"""Launch adapter owned by the independent StarryOS nixosTest framework."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import NamedTuple, Sequence


MANAGED_ROOTFS = (
    "id=disk0,if=none,format=raw,"
    "file=${workspace}/tmp/axbuild/rootfs/rootfs-x86_64-nixos.img"
)


class QemuConfig(NamedTuple):
    args: list[str]
    uefi: bool
    to_bin: bool


class LaunchPlan(NamedTuple):
    qemu: Path
    args: tuple[str, ...]

    def exec_argv(self) -> list[str]:
        return [str(self.qemu), *self.args]


def load_qemu_config(path: Path) -> QemuConfig:
    with path.open("rb") as config_file:
        data = tomllib.load(config_file)
    args = data.get("args")
    if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
        raise ValueError(f"{path} must define a string array named `args`")
    uefi = data.get("uefi")
    to_bin = data.get("to_bin")
    if uefi is not True or to_bin is not True:
        raise ValueError(f"{path} must keep `uefi = true` and `to_bin = true`")
    return QemuConfig(args=list(args), uefi=uefi, to_bin=to_bin)


def replace_managed_rootfs(
    args: Sequence[str],
    overlay: Path,
) -> list[str]:
    replaced = 0
    result: list[str] = []
    for index, arg in enumerate(args):
        is_drive_value = index > 0 and args[index - 1] == "-drive"
        if is_drive_value and arg == MANAGED_ROOTFS:
            replaced += 1
            result.append(f"id=disk0,if=none,format=qcow2,file={overlay}")
            continue
        if is_drive_value and "id=disk0" in arg:
            raise ValueError(f"conflicting disk0 definition: {arg}")
        if "${workspace}" in arg:
            raise ValueError(f"unsupported workspace substitution: {arg}")
        result.append(arg)

    if replaced != 1:
        raise ValueError(
            f"expected exactly one canonical managed rootfs drive, found {replaced}"
        )
    return result


def ensure_overlay_absent(overlay: Path) -> None:
    if overlay.exists():
        raise FileExistsError(f"rootfs overlay already exists: {overlay}")


def build_qemu_command(
    qemu: Path,
    base_args: Sequence[str],
    boot_args: Sequence[str],
    driver_args: Sequence[str],
) -> list[str]:
    return [str(qemu), *base_args, *boot_args, *driver_args]


def parse_args(argv: Sequence[str]) -> tuple[argparse.Namespace, list[str]]:
    argv = list(argv)
    if "--" not in argv:
        raise ValueError("launcher arguments must separate driver QEMU arguments with `--`")
    separator = argv.index("--")
    launcher_args = argv[:separator]
    driver_args = argv[separator + 1 :]

    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu-config", type=Path, required=True)
    parser.add_argument("--qemu", type=Path, required=True)
    parser.add_argument("--qemu-img", type=Path, required=True)
    parser.add_argument("--ovmf-code", type=Path, required=True)
    parser.add_argument("--ovmf-vars", type=Path, required=True)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--rootfs", type=Path, required=True)
    parser.add_argument("--run-dir", type=Path, required=True)
    return parser.parse_args(launcher_args), driver_args


def prepare_launch_plan(
    args: argparse.Namespace,
    driver_args: Sequence[str],
) -> LaunchPlan:
    for path, label in [
        (args.qemu, "QEMU executable"),
        (args.qemu_img, "qemu-img executable"),
        (args.ovmf_code, "OVMF code"),
        (args.ovmf_vars, "OVMF vars template"),
        (args.kernel, "StarryOS kernel"),
        (args.rootfs, "StarryNixOS rootfs"),
    ]:
        require_nonempty_file(path, label)

    args.run_dir.mkdir(parents=True, exist_ok=True)
    overlay = args.run_dir / "starry-nixos-rootfs.qcow2"
    ensure_overlay_absent(overlay)
    subprocess.run(
        [
            str(args.qemu_img),
            "create",
            "-f",
            "qcow2",
            "-F",
            "raw",
            "-b",
            str(args.rootfs),
            str(overlay),
        ],
        check=True,
    )

    ovmf_vars = args.run_dir / "starry-nixos-vars.fd"
    if ovmf_vars.exists():
        raise FileExistsError(f"OVMF vars copy already exists: {ovmf_vars}")
    shutil.copyfile(args.ovmf_vars, ovmf_vars)

    esp = args.run_dir / "starry-nixos.esp"
    boot_dir = esp / "EFI" / "BOOT"
    boot_dir.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(args.kernel, boot_dir / "BOOTX64.EFI")

    config = load_qemu_config(args.qemu_config)
    base_args = replace_managed_rootfs(config.args, overlay)
    boot_args = [
        "-drive",
        f"if=pflash,format=raw,unit=0,readonly=on,file={args.ovmf_code}",
        "-drive",
        f"if=pflash,format=raw,unit=1,file={ovmf_vars}",
        "-drive",
        f"format=raw,file=fat:rw:{esp}",
    ]
    command = build_qemu_command(args.qemu, base_args, boot_args, driver_args)
    return LaunchPlan(qemu=args.qemu, args=tuple(command[1:]))


def require_nonempty_file(path: Path, label: str) -> None:
    if not path.is_file():
        raise FileNotFoundError(f"{label} is missing: {path}")
    if path.stat().st_size == 0:
        raise ValueError(f"{label} is empty: {path}")


def main(argv: Sequence[str] | None = None) -> None:
    parsed, driver_args = parse_args(sys.argv[1:] if argv is None else argv)
    plan = prepare_launch_plan(parsed, driver_args)
    os.execv(plan.qemu, plan.exec_argv())


if __name__ == "__main__":
    main()
