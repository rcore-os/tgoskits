# StarryOS syscall 探针 — 日常用法（本地）

在仓库根目录执行；交叉编译器默认 **`riscv64-linux-musl-gcc`**（可通过 **`CC`** 覆盖）。

## 1. 快速检查（无 QEMU user）

```sh
./scripts/starryos-probes-ci.sh
```

## 2. Linux oracle（需 `qemu-riscv64`）

```sh
CC=riscv64-linux-musl-gcc test-suit/starryos/scripts/build-probes.sh
VERIFY_STRICT=1 test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
```

单个探针：`…/run-diff-probes.sh verify-oracle <basename>`

**轨 B（guest 真内核）**：固定锚点为 **Alpine Linux 3.23.3 / Linux 6.18 LTS**，见 **`docs/starryos-linux-guest-oracle-pin.md`**；金色行目录 **`expected/guest-alpine323/`**（接入 `run_linux_guest_oracle.sh` 后使用）。

## 3. StarryOS QEMU（单核，默认 TOML）

```sh
./test-suit/starryos/scripts/run-starry-probe-qemu.sh write_stdout
```

## 4. 串口日志 vs oracle

```sh
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-write_stdout-0 \
  --timeout 120 \
  2>&1 | tee serial.log

test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout serial.log
```

无文件时：`verify-guest-log-oracle.sh <probe>` 后粘贴日志，**Ctrl+D** 结束。

## 5. SMP 冒烟（`-smp 2`）

使用 **`qemu-riscv64-smp2.toml`**（由 xtask **`--qemu-config`** 指定）：

```sh
./test-suit/starryos/scripts/run-starry-probe-qemu-smp2.sh write_stdout
```

或手写：

```sh
cargo xtask starry test qemu --target riscv64 \
  --qemu-config test-suit/starryos/qemu-riscv64-smp2.toml \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-write_stdout-0 \
  --timeout 120
```

## 6. SMP2 + 全 contract 矩阵（guest 串口 vs oracle）

对每个 **`list-contract-probes.sh`** 中的探针依次 SMP2 启动、写日志、**`verify-guest-log-oracle.sh`**。若基准盘 **`target/riscv64gc-unknown-none-elf/rootfs-riscv64.img`** 不存在，矩阵会先跑 **`cargo xtask starry rootfs --arch riscv64`**（与 **`prepare-rootfs-with-probe.sh`** 共用 **`ensure-starry-base-rootfs.sh`**）。

```sh
test-suit/starryos/scripts/run-smp2-guest-matrix.sh
# 或单探针：…/run-smp2-guest-matrix.sh pipe2_nullfd
```

默认日志目录：**`${TMPDIR:-/tmp}/starry-smp2-matrix/`**。无法取得基准盘时 **exit 2**。可选环境变量：**`STARRY_REFRESH_ROOTFS=1`**、**`SKIP_STARRY_ROOTFS_FETCH=1`**。详见 **`docs/starryos-syscall-smp-notes.md`**。

## 7. 日志文件与 Git

本机 **`serial.log`**、**`serial-*.log`** 已在 **`.gitignore`**，避免误提交。

GitHub 上 **SMP2 全量 guest 矩阵**（定时 + 手动）：工作流 **`starryos-probes-smp2-matrix.yml`**（见 **`docs/starryos-syscall-testing-method.md`**）。

更完整的说明见 **`test-suit/starryos/probes/README.md`** 与 **`docs/starryos-syscall-testing-method.md`**。
