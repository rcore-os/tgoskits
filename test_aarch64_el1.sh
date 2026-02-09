#!/bin/bash
# $env:KERNEL_BUILTIN_CMDLINE = "earlycon=pl011,mmio32,0x9000000"
ostool run -c ./test-suit/timer/aarch64.toml qemu -q ./test-suit/timer/qemu-aarch64.toml