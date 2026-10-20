#!/usr/bin/env python3
"""Exercise the LoongArch axstd control plane through QEMU's host forward."""

import argparse
import hashlib
import json
import re
import socket
import struct
import time
from pathlib import Path


def request(host, port, method, path, body=b"", headers=None, allow_empty=False):
    headers = dict(headers or {})
    headers.setdefault("Host", "localhost")
    headers.setdefault("Connection", "close")
    headers["Content-Length"] = str(len(body))
    payload = [f"{method} {path} HTTP/1.1\r\n".encode()]
    payload.extend(f"{key}: {value}\r\n".encode() for key, value in headers.items())
    payload.append(b"\r\n")
    payload.append(body)
    with socket.create_connection((host, port), timeout=8) as stream:
        stream.settimeout(8)
        stream.sendall(b"".join(payload))
        chunks = []
        while True:
            try:
                chunk = stream.recv(8192)
            except socket.timeout:
                break
            if not chunk:
                break
            chunks.append(chunk)
    response = b"".join(chunks)
    if not response and allow_empty:
        return 0, None, b""
    head, separator, response_body = response.partition(b"\r\n\r\n")
    if not separator:
        raise RuntimeError(f"incomplete HTTP response: {response!r}")
    status_line = head.split(b"\r\n", 1)[0].decode("ascii", "replace")
    status = int(status_line.split()[1])
    try:
        value = json.loads(response_body)
    except json.JSONDecodeError:
        value = None
    return status, value, response_body


def wait_status(host, port, predicate, timeout=180):
    deadline = time.monotonic() + timeout
    last_error = "no response"
    while time.monotonic() < deadline:
        try:
            status, value, _ = request(host, port, "GET", "/api/v1/status")
            if status == 200 and isinstance(value, dict) and predicate(value):
                return value
            last_error = f"status={status} value={value!r}"
        except (OSError, RuntimeError, ValueError) as error:
            last_error = str(error)
        time.sleep(0.25)
    raise RuntimeError(f"LoongArch loader status timeout: {last_error}")


def expect(actual, expected, description, value=None):
    if actual != expected:
        raise RuntimeError(f"{description}: expected {expected}, got {actual}: {value!r}")


def reset_qemu(path):
    """Reset the real VM without invoking the loader's OTA commit path."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(10)
        stream.connect(str(path))
        with stream.makefile("rwb", buffering=0) as channel:
            greeting = json.loads(channel.readline(65536))
            if "QMP" not in greeting:
                raise RuntimeError(f"invalid QMP greeting: {greeting!r}")
            for command in ("qmp_capabilities", "system_reset"):
                channel.write(json.dumps({"execute": command, "id": command}).encode() + b"\n")
                while True:
                    response = json.loads(channel.readline(65536))
                    if response.get("id") == command:
                        if "error" in response:
                            raise RuntimeError(f"QMP {command}: {response['error']!r}")
                        break


def stage_trial(args, epoch, image, digest):
    response = request(
        args.host, args.port, "PUT", "/api/v1/ota/image", image,
        {"X-Boot-Epoch": epoch, "X-Image-Sha256": digest},
        allow_empty=True,
    )
    if response[0] not in (0, 202):
        raise RuntimeError(f"valid OTA image staged: expected 202 or reset EOF, got {response}")
    update_id = response[1].get("update_id") if response[1] else None
    return wait_status(
        args.host, args.port,
        lambda value: value.get("ota", {}).get("trial")
        and (update_id is None or value.get("ota", {}).get("pending_update_id") == update_id),
    )


def returning_efi_image(efi_status):
    """A cooperative EFI entry which preserves the caller and returns a status."""
    # Assembled and disassembled for LoongArch: ori $a0, $zero, imm12; ret.
    if not 0 <= efi_status <= 0xFFF:
        raise ValueError("EFI test status does not fit ori")
    code = struct.pack("<II", 0x03800004 | (efi_status << 10), 0x4C000020)
    address = 0xFFFFFFFF80000000
    code_offset, symbols_offset, strings_offset, sections_offset = 120, 128, 176, 192
    strings = b"\0efi_pe_entry\0"
    image = bytearray(sections_offset + 4 * 64)
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    struct.pack_into("<16sHHIQQQIHHHHHH", image, 0, ident, 2, 258, 1, address,
                     64, sections_offset, 0, 64, 56, 1, 64, 4, 0)
    struct.pack_into("<IIQQQQQQ", image, 64, 1, 5, code_offset, address, address,
                     len(code), len(code), 1)
    image[code_offset:code_offset + len(code)] = code
    struct.pack_into("<IBBHQQ", image, symbols_offset + 24, 1, 0x12, 0, 1,
                     address, len(code))
    image[strings_offset:strings_offset + len(strings)] = strings
    sections = [
        (0, 1, 6, address, code_offset, len(code), 0, 0, 4, 0),
        (0, 2, 0, 0, symbols_offset, 48, 3, 1, 8, 24),
        (0, 3, 0, 0, strings_offset, len(strings), 0, 0, 1, 0),
    ]
    for index, section in enumerate(sections, 1):
        struct.pack_into("<IIQQQQIIQQ", image, sections_offset + index * 64, *section)
    return bytes(image)


def exercise_returned_target(args, status, efi_status):
    epoch = status["boot_epoch"]
    image = returning_efi_image(efi_status)
    digest = hashlib.sha256(image).hexdigest()
    manifest = {
        "boot_id": "loong-returning-efi", "arch": "loongarch64", "image_format": "elf64",
        "kernel": {"size": len(image), "sha256": digest}, "entry_symbol": "efi_pe_entry",
    }
    headers = {"X-Boot-Epoch": epoch}
    response = request(args.host, args.port, "POST", "/api/v1/boot/jobs",
                       json.dumps(manifest).encode(), headers)
    expect(response[0], 201, "returning target manifest", response[1])
    path = "/api/v1/boot/jobs/loong-returning-efi"
    response = request(args.host, args.port, "PUT", path + "/kernel", image,
                       {**headers, "X-Image-Sha256": digest})
    expect(response[0], 200, "returning target upload", response[1])
    binding = {"serial_id": epoch, "binding_id": "1" * 32, "mode": "direct"}
    response = request(args.host, args.port, "POST", "/api/v1/serial/continue",
                       json.dumps(binding).encode(), headers)
    expect(response[0], 200, "returning target serial continue", response[1])
    log_offset = len(args.qemu_log.read_text(errors="replace")) if args.qemu_log else 0
    response = request(args.host, args.port, "POST", path + "/start",
                       headers={**headers, "X-Serial-Binding": binding["binding_id"]},
                       allow_empty=True)
    if response[0] not in (0, 202):
        raise RuntimeError(f"returning EFI entry was rejected: {response!r}")
    restarted = wait_status(args.host, args.port, lambda value: value["boot_epoch"] != epoch)
    expect(restarted["ota"], status["ota"], "returning target reset preserves stable OTA state")
    if args.qemu_log:
        marker = ("efi_handoff_entry_returned: SUCCESS" if efi_status == 0 else
                  f"efi_handoff_entry_failed: EFI handoff failed (EFI status {efi_status:#x})")
        if marker not in args.qemu_log.read_text(errors="replace")[log_offset:]:
            raise RuntimeError(f"returning EFI target reset without its diagnostic: {marker}")
    outcome = ("is reported before reset" if args.qemu_log else
               "resets with unchanged stable OTA state")
    print(f"loong axloader: returning EFI status {efi_status:#x} {outcome}", flush=True)
    return restarted


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=3999)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--elf-image", type=Path, required=True)
    parser.add_argument("--initramfs", type=Path, required=True)
    parser.add_argument("--qmp-socket", type=Path, required=True)
    parser.add_argument("--qemu-log", type=Path, help="verify every loader image in this firmware boot")
    parser.add_argument("--storage-full", action="store_true",
                        help="expect a valid upload to fail on a pre-filled FAT volume")
    args = parser.parse_args()

    # Build both images before starting QEMU. This timeout covers firmware and
    # loader startup; cold compilation has its own CI command and failure.
    status = wait_status(
        args.host,
        args.port,
        lambda value: value.get("arch") == "loongarch64",
        timeout=300,
    )
    epoch = status["boot_epoch"]
    response = request(args.host, args.port, "GET", "/api/v1/ota/status")
    expect(response[0], 200, "read-only OTA status needs no epoch", response[1])
    response = request(
        args.host, args.port, "GET", "/api/v1/status",
        headers={"X-Boot-Epoch": epoch, "x-boot-epoch": epoch},
    )
    expect(response[0], 400, "ambiguous control headers rejected", response[1])
    headers = {"X-Boot-Epoch": epoch, "Content-Type": "application/json"}
    bad_kernel = b"\x07"
    manifest = {
        "boot_id": "loong-http-smoke",
        "arch": "loongarch64",
        "image_format": "elf64",
        "kernel": {
            "size": len(bad_kernel),
            "sha256": hashlib.sha256(bad_kernel).hexdigest(),
        },
        "entry_symbol": "efi_pe_entry",
    }
    encoded = json.dumps(manifest, separators=(",", ":")).encode()
    response = request(args.host, args.port, "POST", "/api/v1/boot/jobs", encoded, headers)
    expect(response[0], 201, "Loong manifest create", response[1])
    response = request(args.host, args.port, "GET", "/api/v1/boot/jobs/loong-http-smoke")
    expect(response[0], 200, "read-only job status needs no epoch", response[1])
    response = request(args.host, args.port, "POST", "/api/v1/boot/jobs", encoded, headers)
    expect(response[0], 200, "identical manifest retry", response[1])
    upload_headers = {
        "X-Boot-Epoch": epoch,
        "X-Image-Sha256": hashlib.sha256(bad_kernel).hexdigest(),
    }
    response = request(
        args.host,
        args.port,
        "PUT",
        "/api/v1/boot/jobs/loong-http-smoke/kernel",
        bad_kernel,
        upload_headers,
    )
    expect(response[0], 200, "bad ELF upload accepted for validation", response[1])
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/boot/jobs/loong-http-smoke/start",
        headers={"X-Boot-Epoch": epoch},
    )
    expect(response[0], 409, "unbound start rejected", response[1])
    expect(response[1], {"error": "serial_binding_required"}, "unbound start reason")
    binding = {
        "serial_id": epoch,
        "binding_id": "0" * 32,
        "mode": "direct",
    }
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/serial/continue",
        json.dumps(binding, separators=(",", ":")).encode(),
        headers,
    )
    expect(response[0], 200, "direct serial continue", response[1])
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/boot/jobs/loong-http-smoke/start",
        headers={**headers, "X-Serial-Binding": binding["binding_id"]},
    )
    expect(response[0], 409, "invalid ELF handoff rejected", response[1])
    expect(response[1], {"error": "kernel_load_failed"}, "invalid ELF handoff reason")
    response = request(
        args.host,
        args.port,
        "DELETE",
        "/api/v1/boot/jobs/loong-http-smoke",
        headers={"X-Boot-Epoch": epoch},
    )
    expect(response[0], 204, "boot transaction cancellation", response[1])

    response = request(
        args.host,
        args.port,
        "PUT",
        "/api/v1/ota/image",
        bad_kernel,
        {
            "X-Boot-Epoch": epoch,
            "X-Image-Sha256": hashlib.sha256(bad_kernel).hexdigest(),
        },
    )
    expect(response[0], 422, "invalid OTA image rejected", response[1])

    image = bytearray(args.image.read_bytes())
    # Give the trial a distinct file identity without changing executable code.
    # Otherwise identical A/B images legitimately continue in the firmware-loaded
    # copy and the test would never exercise the inherited slot entry.
    pe_offset = struct.unpack_from("<I", image, 0x3c)[0]
    timestamp = struct.unpack_from("<I", image, pe_offset + 8)[0]
    struct.pack_into("<I", image, pe_offset + 8, timestamp ^ 1)
    image = bytes(image)
    image_digest = hashlib.sha256(image).hexdigest()
    if args.storage_full:
        response = request(
            args.host, args.port, "PUT", "/api/v1/ota/image", image,
            {"X-Boot-Epoch": epoch, "X-Image-Sha256": image_digest},
        )
        expect(response[0], 409, "full FAT storage failure", response[1])
        expect(response[1], {"error": "ota_storage_failed"}, "storage failure reason")
        unchanged = wait_status(args.host, args.port, lambda value: value["boot_epoch"] == epoch)
        expect(unchanged["ota"], status["ota"], "failed upload preserves OTA state")
        print("loong axloader full FAT: storage failure is 409 and stable state is preserved")
        return

    trial = stage_trial(args, epoch, image, image_digest)
    failed_update_id = trial["ota"]["pending_update_id"]
    response = request(args.host, args.port, "GET", "/api/v1/ota/status")
    expect(response[0], 200, "trial status remains readable without epoch", response[1])
    trial_epoch = trial["boot_epoch"]
    reset_qemu(args.qmp_socket)
    rolled_back = wait_status(
        args.host, args.port,
        lambda value: value["boot_epoch"] != trial_epoch
        and not value.get("ota", {}).get("trial")
        and value.get("ota", {}).get("pending_update_id") is None
        and value.get("ota", {}).get("last_outcome") == "rolled_back",
        timeout=300,
    )
    expect(rolled_back["ota"]["running_sha256"], status["ota"]["active_sha256"],
           "unconfirmed trial returns to stable slot")
    response = request(
        args.host, args.port, "POST", "/api/v1/ota/confirm",
        json.dumps({"update_id": failed_update_id}, separators=(",", ":")).encode(),
        {"X-Boot-Epoch": rolled_back["boot_epoch"], "Content-Type": "application/json"},
    )
    expect(response[0], 409, "rolled-back trial cannot be confirmed", response[1])
    trial = stage_trial(args, rolled_back["boot_epoch"], image, image_digest)
    update_id = trial["ota"]["pending_update_id"]
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/ota/confirm",
        json.dumps({"update_id": update_id}, separators=(",", ":")).encode(),
        {"X-Boot-Epoch": trial["boot_epoch"], "Content-Type": "application/json"},
    )
    expect(response[0], 200, "OTA trial confirmation", response[1])
    confirmed = wait_status(
        args.host,
        args.port,
        lambda value: not value.get("ota", {}).get("trial")
        and value.get("ota", {}).get("pending_update_id") is None,
    )
    expect(confirmed["ota"]["active_sha256"], image_digest, "confirmed stable digest")
    reset_qemu(args.qmp_socket)
    stable = wait_status(
        args.host, args.port,
        lambda value: value["boot_epoch"] != confirmed["boot_epoch"]
        and not value.get("ota", {}).get("trial")
        and value.get("ota", {}).get("pending_update_id") is None
        and value.get("ota", {}).get("active_sha256") == image_digest
        and value.get("ota", {}).get("running_sha256") == image_digest
        and value.get("ota", {}).get("last_outcome") == "confirmed",
        timeout=300,
    )
    for efi_status in (0, 1):
        stable = exercise_returned_target(args, stable, efi_status)

    # The target must be an independent ArceOS EFI application. Reusing the
    # loader ELF would make its inherited image range fail the loader's PE
    # self-image check before the payload handoff can be observed.
    elf_image_path = args.elf_image
    initramfs_path = args.initramfs
    elf_image = elf_image_path.read_bytes()
    initramfs = initramfs_path.read_bytes()
    positive_epoch = wait_status(
        args.host,
        args.port,
        lambda value: not value.get("ota", {}).get("trial"),
    )["boot_epoch"]
    positive_cmdline = "axloader-positive=1"
    if args.qemu_log:
        # A machine reset starts a new firmware lifetime. Earlier boots' image
        # addresses can now belong to firmware, so use only the current boot.
        text = args.qemu_log.read_text(errors="replace")
        records = re.findall(
            r"\baxloader_memory inherited=([01]) "
            r"start=(0x[0-9a-fA-F]+) bytes=(0x[0-9a-fA-F]+)", text)
        cold = [index for index, record in enumerate(records) if record[0] == "0"]
        if not cold:
            raise RuntimeError("missing runtime memory report for the current firmware boot")
        images = sorted({(int(start, 16), int(start, 16) + int(size, 16))
                         for _, start, size in records[cold[-1]:]})
        if len(images) < 2:
            raise RuntimeError(f"missing firmware-loader and OTA-loader memory evidence: {images}")
        positive_cmdline += "".join(f" reclaim-image={start:x}:{end - start:x}" for start, end in images)
        print(f"loong axloader: require ordinary page reuse from {len(images)} loader images", flush=True)
    positive_manifest = {
        "boot_id": "loong-valid-elf",
        "arch": "loongarch64",
        "image_format": "elf64",
        "kernel": {
            "size": len(elf_image),
            "sha256": hashlib.sha256(elf_image).hexdigest(),
        },
        "initramfs": {
            "size": len(initramfs),
            "sha256": hashlib.sha256(initramfs).hexdigest(),
        },
        "cmdline": positive_cmdline,
        "entry_symbol": "efi_pe_entry",
    }
    positive_headers = {
        "X-Boot-Epoch": positive_epoch,
        "Content-Type": "application/json",
    }
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/boot/jobs",
        json.dumps(positive_manifest, separators=(",", ":")).encode(),
        positive_headers,
    )
    expect(response[0], 201, "valid Loong ELF manifest", response[1])
    for kind, image, digest in (
        ("kernel", elf_image, positive_manifest["kernel"]["sha256"]),
        ("initramfs", initramfs, positive_manifest["initramfs"]["sha256"]),
    ):
        response = request(
            args.host,
            args.port,
            "PUT",
            f"/api/v1/boot/jobs/loong-valid-elf/{kind}",
            image,
            {"X-Boot-Epoch": positive_epoch, "X-Image-Sha256": digest},
        )
        expect(response[0], 200, f"valid Loong ELF {kind} upload", response[1])
    binding = {
        "serial_id": positive_epoch,
        "binding_id": "0" * 32,
        "mode": "direct",
    }
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/serial/continue",
        json.dumps(binding, separators=(",", ":")).encode(),
        positive_headers,
    )
    expect(response[0], 200, "valid Loong ELF direct serial continue", response[1])
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/boot/jobs/loong-valid-elf/start",
        headers={
            "X-Boot-Epoch": positive_epoch,
            "X-Serial-Binding": binding["binding_id"],
        },
    )
    expect(response[0], 202, "valid Loong ELF handoff accepted", response[1])
    print(
        "loong axloader HTTP/OTA/ELF smoke: status, idempotent boot, serial gate, "
        "ELF rejection, valid ELF handoff, stage, reset, rollback, confirm persistence passed"
    )


if __name__ == "__main__":
    main()
