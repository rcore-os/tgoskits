#!/usr/bin/env python3
"""Run bug verification tests on StarryOS via QEMU TCP serial - one test per boot."""

import datetime
import os
import socket
import subprocess
import sys
import time

PROMPT = "root@starry"

def recv_until(s, marker, timeout=10):
    data = ""
    start = datetime.datetime.now()
    while True:
        try:
            b = s.recv(4096).decode("utf-8", errors="ignore")
            if b:
                data += b
                print(b, end="", flush=True)
        except socket.timeout:
            pass
        except ConnectionError:
            break
        if marker in data:
            break
        if datetime.datetime.now() - start > datetime.timedelta(seconds=timeout):
            break
    return data

def send_and_wait(s, cmd, wait=3):
    print(f"\n>>> Sending: {cmd}")
    try:
        s.sendall((cmd + "\n").encode())
    except BrokenPipeError:
        return "BROKEN_PIPE"
    time.sleep(wait)
    try:
        result = recv_until(s, PROMPT, timeout=5)
    except ConnectionError:
        result = "CONNECTION_LOST"
    return result

def run_qemu_with_test(test_cmd, label):
    """Start QEMU, run a single test, return result."""
    workspace = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    disk_img = os.path.join(workspace, "make", "disk.img")
    kernel = os.path.join(workspace, "StarryOS_riscv64-qemu-virt.bin")

    qemu_cmd = [
        "qemu-system-riscv64",
        "-m", "1G", "-smp", "1",
        "-machine", "virt",
        "-bios", "default",
        "-kernel", kernel,
        "-device", "virtio-blk-pci,drive=disk0",
        "-drive", f"id=disk0,if=none,format=raw,file={disk_img}",
        "-device", "virtio-net-pci,netdev=net0",
        "-netdev", "user,id=net0,hostfwd=tcp::5555-:5555,hostfwd=udp::5555-:5555",
        "-nographic",
        "-monitor", "none",
        "-serial", "tcp::4444,server=on",
    ]

    print(f"\n{'='*60}")
    print(f"Testing: {label}")
    print(f"{'='*60}")
    
    p = subprocess.Popen(qemu_cmd, stderr=subprocess.PIPE, text=True)
    
    for attempt in range(10):
        try:
            time.sleep(2)
            s = socket.create_connection(("localhost", 4444), timeout=5)
            break
        except (ConnectionRefusedError, socket.timeout):
            if p.poll() is not None:
                print(f"QEMU exited prematurely with code {p.returncode}")
                return "QEMU_EXITED"
    else:
        print("Failed to connect")
        p.kill()
        return "CONNECT_FAILED"

    s.settimeout(2)

    # Wait for shell prompt
    buffer = recv_until(s, PROMPT, timeout=30)
    if PROMPT not in buffer:
        p.kill()
        return "NO_PROMPT"

    time.sleep(1)
    
    # Disable job control
    s.sendall(b"set +m\n")
    time.sleep(1)
    recv_until(s, PROMPT, timeout=3)

    # Run the test
    result = send_and_wait(s, test_cmd, wait=5)
    
    # Try to exit cleanly
    try:
        s.sendall(b"exit\n")
        time.sleep(1)
        s.close()
    except:
        pass

    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        p.kill()

    return result

def main():
    branch = subprocess.check_output(["git", "rev-parse", "--abbrev-ref", "HEAD"]).decode().strip()
    print(f"Current branch: {branch}")
    
    tests = [
        ("Bug #2: robust mutex owner death", "/root/test_robust_mutex; echo EXIT=$?"),
        ("Bug #3: fcntl F_GETFL", "/root/test_fcntl_getfl; echo EXIT=$?"),
        ("Bug #5: TIOCSGRP/tcsetpgrp", "/root/test_tiocspgrp; echo EXIT=$?"),
        ("Bug #6: mremap shared", "/root/test_mremap_shared; echo EXIT=$?"),
        ("Bug #8: directory read/write", "/root/test_directory_read_write; echo EXIT=$?"),
        ("Bug #9: lseek pipe ESPIPE", "/root/test_lseek_pipe_espipe; echo EXIT=$?"),
    ]
    
    results = {}
    for label, cmd in tests:
        result = run_qemu_with_test(cmd, label)
        results[label] = result
        time.sleep(2)  # Brief pause between QEMU instances

    print("\n\n" + "="*60)
    print(f"TEST RESULTS SUMMARY ({branch} branch)")
    print("="*60)
    
    for label, result in results.items():
        print(f"\n--- {label} ---")
        clean = result.replace('\x1b[6n', '').replace('\x1b', '')
        if "panic" in clean.lower():
            status = "💥 KERNEL PANIC"
        elif "PASS" in clean and "FAIL" not in clean:
            status = "✅ PASSED"
        elif "FAIL" in clean:
            status = "❌ FAILED"
        elif "BROKEN_PIPE" in clean or "CONNECTION_LOST" in clean:
            status = "💥 KERNEL CRASHED"
        else:
            status = "❓ UNKNOWN"
        
        # Show relevant output
        lines = clean.strip().split('\n')
        relevant = [l for l in lines if any(k in l for k in ['PASS', 'FAIL', 'panic', 'EXIT=', 'Hello', 'All tests'])]
        if relevant:
            for line in relevant[-10:]:
                print(f"  {line}")
        else:
            for line in lines[-5:]:
                print(f"  {line}")
        print(f"  Status: {status}")

if __name__ == "__main__":
    main()
