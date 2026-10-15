#!/usr/bin/env python3
"""Exercise the LoongArch axstd control plane through QEMU's host forward."""

import argparse
import hashlib
import json
import socket
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


def expect(status, expected, description, value=None):
    if status != expected:
        raise RuntimeError(f"{description}: expected {expected}, got {status}: {value!r}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=3999)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--elf-image", type=Path, required=True)
    parser.add_argument("--initramfs", type=Path, required=True)
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

    image = args.image.read_bytes()
    image_digest = hashlib.sha256(image).hexdigest()
    response = request(
        args.host,
        args.port,
        "PUT",
        "/api/v1/ota/image",
        image,
        {"X-Boot-Epoch": epoch, "X-Image-Sha256": image_digest},
        allow_empty=True,
    )
    if response[0] not in (0, 202):
        raise RuntimeError(f"valid OTA image staged: expected 202 or reset EOF, got {response}")
    update_id = response[1].get("update_id") if response[1] else None
    trial = wait_status(
        args.host,
        args.port,
        lambda value: value.get("ota", {}).get("trial")
        and (update_id is None or value.get("ota", {}).get("pending_update_id") == update_id),
    )
    update_id = update_id or trial["ota"]["pending_update_id"]
    response = request(
        args.host,
        args.port,
        "POST",
        "/api/v1/ota/confirm",
        json.dumps({"update_id": update_id}, separators=(",", ":")).encode(),
        {"X-Boot-Epoch": trial["boot_epoch"], "Content-Type": "application/json"},
    )
    expect(response[0], 200, "OTA trial confirmation", response[1])
    wait_status(
        args.host,
        args.port,
        lambda value: not value.get("ota", {}).get("trial")
        and value.get("ota", {}).get("pending_update_id") is None,
    )

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
        "cmdline": "axloader-positive=1",
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
        "ELF rejection, valid ELF handoff, stage, reset, confirm passed"
    )


if __name__ == "__main__":
    main()
