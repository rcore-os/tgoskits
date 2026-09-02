#!/usr/bin/env python3
"""Freeze and validate one physical-board StarryOS RT formal campaign."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Mapping, Sequence

import formal_campaign_contract as contract
import formal_campaign_receipt as receipts


# Keep the import surface used by the focused contract tests while the CLI
# remains a thin orchestration layer.
ARTIFACT_NAMES = contract.ARTIFACT_NAMES
FROZEN_SOURCE_PATHS = contract.FROZEN_SOURCE_PATHS
ContractError = contract.ContractError
Slot = contract.Slot
build_preregistration = contract.build_preregistration
sha256_file = contract.sha256_file
validate_preregistration = contract.validate_preregistration
validate_stage_log = contract.validate_stage_log
validate_summary = contract.validate_summary
build_receipt = receipts.build_receipt
campaign_slots = receipts.campaign_slots
next_slot = receipts.next_slot
receipt_path = receipts.receipt_path


def add_common_preregistration(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--preregistration", type=Path, required=True)


def parse_arguments(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    preregister = commands.add_parser("preregister")
    preregister.add_argument("--workspace", type=Path, required=True)
    preregister.add_argument("--expected-commit", required=True)
    preregister.add_argument("--source-ref", required=True)
    preregister.add_argument("--board-type", required=True)
    preregister.add_argument("--service-id", required=True)
    preregister.add_argument("--hardware-id", required=True)
    preregister.add_argument("--hostname", required=True)
    preregister.add_argument("--base-rootfs", type=Path, required=True)
    preregister.add_argument("--host-toolchain", type=Path, required=True)
    preregister.add_argument("--probe", type=Path, required=True)
    preregister.add_argument("--pair-kernel", type=Path, required=True)
    preregister.add_argument("--pair-rootfs", type=Path, required=True)
    preregister.add_argument("--soak-kernel", type=Path, required=True)
    preregister.add_argument("--soak-rootfs", type=Path, required=True)
    preregister.add_argument("--guest-dtb", type=Path, required=True)
    preregister.add_argument("--pair-timeout", type=int, default=900)
    preregister.add_argument("--soak-timeout", type=int, default=4500)
    preregister.add_argument("--output", type=Path, required=True)

    verify = commands.add_parser("verify")
    add_common_preregistration(verify)
    verify.add_argument("--allow-dirty", action="store_true")

    stage = commands.add_parser("validate-stage")
    add_common_preregistration(stage)
    stage.add_argument("--stage-log", type=Path, required=True)

    status = commands.add_parser("status")
    add_common_preregistration(status)
    status.add_argument("--result-root", type=Path, required=True)

    receipt = commands.add_parser("write-receipt")
    add_common_preregistration(receipt)
    receipt.add_argument("--result-root", type=Path, required=True)
    receipt.add_argument("--phase", choices=("pair", "soak"), required=True)
    receipt.add_argument("--pair", type=int)
    receipt.add_argument("--profile", choices=contract.SOAK_ORDER, required=True)
    receipt.add_argument("--stage-log", type=Path, required=True)
    receipt.add_argument("--console-log", type=Path, required=True)
    receipt.add_argument("--harvest-log", type=Path, required=True)
    receipt.add_argument("--summary", type=Path, required=True)
    receipt.add_argument("--raw", type=Path, required=True)
    receipt.add_argument("--guest-irq", type=Path, required=True)
    receipt.add_argument("--host-trace", type=Path, required=True)
    receipt.add_argument("--started-at", required=True)
    receipt.add_argument("--finished-at", required=True)
    return parser.parse_args(argv)


def preregistration_artifacts(arguments: argparse.Namespace) -> dict[str, Path]:
    return {
        "base_rootfs": arguments.base_rootfs,
        "host_toolchain": arguments.host_toolchain,
        "probe": arguments.probe,
        "pair_kernel": arguments.pair_kernel,
        "pair_rootfs": arguments.pair_rootfs,
        "soak_kernel": arguments.soak_kernel,
        "soak_rootfs": arguments.soak_rootfs,
        "guest_dtb": arguments.guest_dtb,
    }


def command_status(
    preregistration: Mapping[str, object], result_root: Path
) -> dict[str, object]:
    slots = receipts.campaign_slots()
    next_campaign_slot = receipts.next_slot(preregistration, result_root)
    completed = [
        {
            "phase": slot.phase,
            "pair": slot.pair,
            "profile": slot.profile,
        }
        for slot in slots
        if receipts.receipt_path(result_root, slot).is_file()
    ]
    return {
        "schema_version": 1,
        "completed_count": len(completed),
        "total_count": len(slots),
        "completed": completed,
        "next": (
            None
            if next_campaign_slot is None
            else {
                "phase": next_campaign_slot.phase,
                "pair": next_campaign_slot.pair,
                "profile": next_campaign_slot.profile,
            }
        ),
    }


def execute(arguments: argparse.Namespace) -> int:
    if arguments.command == "preregister":
        document = contract.build_preregistration(
            workspace=arguments.workspace,
            expected_commit=arguments.expected_commit,
            source_ref=arguments.source_ref,
            board_type=arguments.board_type,
            service_id=arguments.service_id,
            hardware_id=arguments.hardware_id,
            hostname=arguments.hostname,
            artifacts=preregistration_artifacts(arguments),
            pair_timeout_seconds=arguments.pair_timeout,
            soak_timeout_seconds=arguments.soak_timeout,
        )
        contract.write_json_exclusive(arguments.output, document)
        print(f"AXVISOR_RT_FORMAL_PREREGISTERED path={arguments.output}")
        return 0

    preregistration = contract.read_json(
        arguments.preregistration, "preregistration"
    )
    contract.validate_preregistration(
        preregistration,
        arguments.workspace,
        require_clean=not getattr(arguments, "allow_dirty", False),
    )
    if arguments.command == "verify":
        print("AXVISOR_RT_FORMAL_INPUTS_VERIFIED")
        return 0
    if arguments.command == "validate-stage":
        stage_text = arguments.stage_log.read_text(encoding="utf-8")
        identity = contract.validate_stage_log(preregistration, stage_text)
        print(
            "AXVISOR_RT_FORMAL_STAGE_VERIFIED "
            f"board_id={identity['board_id']} hostname={identity['hostname']}"
        )
        return 0
    if arguments.command == "status":
        print(
            json.dumps(
                command_status(preregistration, arguments.result_root),
                sort_keys=True,
            )
        )
        return 0
    if arguments.command == "write-receipt":
        if arguments.phase == "pair":
            if arguments.pair not in range(1, 6):
                raise contract.ContractError("pair receipt requires --pair in 1..5")
        elif arguments.pair is not None:
            raise contract.ContractError("soak receipt must omit --pair")
        slot = contract.Slot(arguments.phase, arguments.pair, arguments.profile)
        receipt = receipts.build_receipt(
            preregistration,
            arguments.result_root,
            slot,
            arguments.stage_log,
            arguments.console_log,
            arguments.harvest_log,
            arguments.summary,
            arguments.raw,
            arguments.guest_irq,
            arguments.host_trace,
            arguments.started_at,
            arguments.finished_at,
        )
        output = receipts.receipt_path(arguments.result_root, slot)
        contract.write_json_exclusive(output, receipt)
        print(f"AXVISOR_RT_FORMAL_RECEIPT_WRITTEN path={output}")
        return 0
    raise contract.ContractError(f"unsupported command {arguments.command!r}")


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        return execute(arguments)
    except (contract.ContractError, OSError, UnicodeDecodeError) as error:
        print(f"StarryOS RT formal campaign failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
