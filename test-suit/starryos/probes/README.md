# StarryOS syscall probes

Small static `riscv64-linux-musl` ELF programs for Linux-oracle vs StarryOS diff testing.

## Build (host)

Requires `riscv64-linux-musl-gcc` on PATH (or set `CC`):

```sh
export CC=/path/to/riscv64-linux-musl-gcc   # optional
test-suit/starryos/scripts/build-probes.sh
```

Binaries go to `probes/build-riscv64/`.

## Linux oracle (user-mode QEMU)

Needs `qemu-riscv64` (Debian/Ubuntu: `qemu-user`):

```sh
export QEMU_RV64=qemu-riscv64
test-suit/starryos/scripts/run-diff-probes.sh oracle write_stdout
# 校验全部 expected/*.line（需已 build 且 qemu-riscv64 可用）
test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
# CI：缺少 qemu-user 时失败退出码 2
VERIFY_STRICT=1 test-suit/starryos/scripts/run-diff-probes.sh verify-oracle-all
```

## Contract probes (hand-written)

| Basename | Syscall | `expected/*.line` |
|----------|---------|-------------------|
| `write_stdout` | write(2) 零长度写 stdout | `expected/write_stdout.line` |
| `close_badfd` | close(2) 非法 fd → EBADF | `expected/close_badfd.line` |
| `read_stdin_zero` | read(2) stdin count=0 → 0 | `expected/read_stdin_zero.line` |
| `dup_badfd` | dup(2) 非法 fd → EBADF | `expected/dup_badfd.line` |
| `fcntl_badfd` | fcntl(2) 非法 fd + F_GETFD → EBADF | `expected/fcntl_badfd.line` |

列出当前 contract 名称：`test-suit/starryos/scripts/list-contract-probes.sh`

## Catalog and extract

```sh
python3 scripts/extract_starry_syscalls.py --out-json docs/starryos-syscall-dispatch.json
python3 scripts/extract_starry_syscalls.py --check-catalog docs/starryos-syscall-catalog.yaml
python3 scripts/gen_syscall_probes.py --catalog docs/starryos-syscall-catalog.yaml
```

## Output format

One line per case, machine-parseable, e.g.:

`CASE write_stdout.zero_len ret=0 errno=0 note=handwritten`

## StarryOS side

Copy the static ELF into the guest rootfs (e.g. with `debugfs` on `rootfs-riscv64.img`) and run it from `shell_init_cmd` or an init script; compare stdout with the Linux oracle line above.

## StarryOS QEMU 回归（S0-5）

1. 准备带 probe 的磁盘镜像：

   ```sh
   ./test-suit/starryos/scripts/prepare-rootfs-with-write_stdout-probe.sh
   ```

2. 运行 `starryos-test`（使用 `--test-disk-image` 指向注入后的镜像）：

   ```sh
   cargo xtask starry test qemu --target riscv64 \
     --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe.img \
     --shell-init-cmd test-suit/starryos/testcases/probe-write_stdout-0 \
     --timeout 120
   ```

说明：`xtask` 仍会把该镜像再复制一份为临时测试盘，不会改写 `rootfs-riscv64-probe.img`。

### 其它探针（通用注入）

```sh
./test-suit/starryos/scripts/prepare-rootfs-with-probe.sh close_badfd
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe-close_badfd.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-close_badfd-0 \
  --timeout 120
```

## 一键 QEMU（starryos-test）

在仓库根目录（需已 `cargo xtask starry rootfs --arch riscv64`）：

```sh
./test-suit/starryos/scripts/run-starry-probe-qemu.sh read_stdin_zero
```

## 本地端到端冒烟（步骤 5）

从仓库根目录执行（会下载 rootfs、交叉编译探针、再跑 `cargo xtask starry test qemu`，**耗时较长**，默认 CI 不跑）：

```sh
./test-suit/starryos/scripts/run-e2e-probe-smoke.sh write_stdout
# 或其它探针 basename，例如 read_stdin_zero
```

## GitHub Actions（步骤 3–4）

工作流 **`.github/workflows/starryos-probes.yml`**：

- **static**：`./scripts/starryos-probes-ci.sh`（catalog、覆盖、`sh -n`；若 runner 上无交叉编译器则跳过构建）。
- **linux-oracle**：安装 `python3-yaml`、`qemu-user`、`gcc-riscv64-linux-gnu`，用 **GNU** 交叉链静态编译后执行 **`VERIFY_STRICT=1 verify-oracle-all`**。

在 GitHub：**Actions → StarryOS syscall probes → Run workflow** 可在 `next` 等分支手动触发。

## 串口 / 日志 与 oracle 自动比对（推荐）

从 **QEMU 串口保存的文本**（或任意包含探针输出的日志）里取 **首行** `^CASE `，与 `expected/<探针名>.line` 对比：

```sh
# 日志在文件里
test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout my-serial.log

# 日志在管道里（例如重跑时把 xtask 输出存下来再验）
cargo xtask starry test qemu ... 2>&1 | tee serial.log
test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout serial.log

# 从 stdin 读入（等价于第二个参数写 `-`）
grep . serial.log | test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout -
```

退出码：**0** 一致，**1** 与 oracle 不一致，**2** 日志中找不到 `^CASE ` 行。

底层脚本（按需单独用）：

- `extract-case-line.sh [file]`：只抽取首行 `CASE …`（去掉 `\r`）。
- `diff-guest-line.sh <probe> [line]`：把一行与 `expected/<probe>.line` 比较。

## Catalog 与文件一致性

```sh
python3 scripts/check_probe_coverage.py
./scripts/starryos-probes-ci.sh
```

## 相关文档

- `docs/starryos-syscall-testing-method.md` — 分层与新增 contract 清单
- `docs/starryos-syscall-compat-matrix.yaml` — Linux 对齐矩阵骨架
- `docs/starryos-syscall-smp-notes.md` — SMP / 多核占位说明
- `docs/starryos-syscall-progress-rounds.md` — 多轮迭代纪要
- `docs/starryos-syscall-commit-strategy.md` — 分组提交建议
- `docs/starryos-probes-ci-example.md` — CI / oracle job 示例片段
