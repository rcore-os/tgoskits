# SG2002 StarryOS Boot

This case boots StarryOS on SG2002 with the Linux root filesystem as root.

The current SG2002 remote board profile reports `boot_mode=pxe`, while ostool
0.15.1 only runs the generic board path for `boot_mode=uboot`. Starry's board
command dispatches SG2002 to a board-specific runner:

```bash
cargo xtask starry board \
  --config test-suit/starryos/normal/board-sg2002-riscv64/build-riscv64gc-unknown-none-elf.toml \
  --board-config test-suit/starryos/normal/board-sg2002-riscv64/boot/board-sg2002-riscv64.toml \
  --server 10.3.10.60 \
  --port 2999
```

The runner builds `starryos.uimg`, interrupts U-Boot over serial, uploads the
image with U-Boot `loady`, and boots with:

```text
bootm 0x80200000 - $fdtcontroladdr
```
