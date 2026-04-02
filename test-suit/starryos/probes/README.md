# StarryOS syscall probes

Small static `riscv64-linux-musl` ELF programs for Linux-oracle vs StarryOS diff testing.

**日常用法（本地）**：见 **`docs/starryos-probes-daily.md`**（`starryos-probes-ci.sh`、`verify-oracle-all`、QEMU、`verify-guest-log-oracle`、SMP）。

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
| `openat_badfd` | openat(2) 非法 dirfd + 相对路径 → EBADF | `expected/openat_badfd.line` |
| `openat_enoent` | openat(2) 不存在绝对路径 → ENOENT | `expected/openat_enoent.line` |
| `lseek_badfd` | lseek(2) 非法 fd → EBADF | `expected/lseek_badfd.line` |
| `ioctl_badfd` | ioctl(2) 非法 fd + FIONREAD → EBADF | `expected/ioctl_badfd.line` |
| `pipe2_nullfd` | pipe2(2) pipefd=NULL → EFAULT | `expected/pipe2_nullfd.line` |
| `clock_gettime_null_ts` | clock_gettime(2) timespec=NULL → EFAULT | `expected/clock_gettime_null_ts.line` |
| `execve_enoent` | execve(2) 不存在路径 → ENOENT | `expected/execve_enoent.line` |
| `wait4_echild` | wait4(2) 非子进程 pid → ECHILD | `expected/wait4_echild.line` |
| `futex_wake_nop` | futex FUTEX_WAKE 无等待者 → 0 | `expected/futex_wake_nop.line` |
| `ppoll_zero_fds` | ppoll(2) nfds=0 + 零超时 → 立即返回 | `expected/ppoll_zero_fds.line` |
| `io_zero_rw` | read+write 零长度组合（**多 CASE**） | `expected/io_zero_rw.cases`（无 `.line`） |

列出当前 contract 名称：`test-suit/starryos/scripts/list-contract-probes.sh`

**全 contract 在 SMP2 下跑 guest 并与 oracle 对齐**：`test-suit/starryos/scripts/run-smp2-guest-matrix.sh`（缺 **`rootfs-riscv64.img`** 时会先 **`cargo xtask starry rootfs --arch riscv64`**；见 **`docs/starryos-syscall-smp-notes.md`**）。

## Catalog and extract

```sh
python3 scripts/extract_starry_syscalls.py --out-json docs/starryos-syscall-dispatch.json
python3 scripts/extract_starry_syscalls.py --check-catalog docs/starryos-syscall-catalog.yaml
python3 scripts/gen_syscall_probes.py --catalog docs/starryos-syscall-catalog.yaml
```

## Output format

默认每条探针 **一行** `CASE`，机器可读，例如：

`CASE write_stdout.zero_len ret=0 errno=0 note=handwritten`

若一条探针需要 **多条** `CASE`（顺序可能因调度变化），使用 **`expected/<basename>.cases`**（每行一条 `CASE`，校验时对日志与期望分别 **`sort -u`** 后做集合比较）；**`verify-guest-log-oracle.sh`** 与 **`run-diff-probes.sh verify-oracle`** 会自动识别。勿与 **`.line`** 并存。

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

`openat` 两个 contract 示例（镜像名随探针 basename 变化）：

```sh
./test-suit/starryos/scripts/prepare-rootfs-with-probe.sh openat_badfd
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe-openat_badfd.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-openat_badfd-0 \
  --timeout 120

./test-suit/starryos/scripts/prepare-rootfs-with-probe.sh openat_enoent
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe-openat_enoent.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-openat_enoent-0 \
  --timeout 120
```

## 一键 QEMU（starryos-test）

在仓库根目录（需已 `cargo xtask starry rootfs --arch riscv64`）：

```sh
./test-suit/starryos/scripts/run-starry-probe-qemu.sh read_stdin_zero
```

### SMP（`-smp 2`）

```sh
./test-suit/starryos/scripts/run-starry-probe-qemu-smp2.sh write_stdout
```

底层使用 **`test-suit/starryos/qemu-riscv64-smp2.toml`** 与 **`cargo xtask starry test qemu --qemu-config …`**。

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

重负载 **SMP2 全 contract guest 矩阵**（不在 push 触发）：**`.github/workflows/starryos-probes-smp2-matrix.yml`** — **`workflow_dispatch`** 与 **每日定时**；需 **`ghcr.io/<repo>-container`**（与主仓库 `test.yml` 中 Starry 测试一致）。

在 GitHub：**Actions → StarryOS syscall probes → Run workflow** 可在 `next` 等分支手动触发；SMP2 矩阵在 **StarryOS probes SMP2 guest matrix** 工作流中单独触发。

## 串口 / 日志 与 oracle 自动比对（推荐）

从 **QEMU 串口保存的文本**（或任意包含探针输出的日志）里取 **首行** `^CASE `，与 `expected/<探针名>.line` 对比：

```sh
# 方式 A：没有现成文件时——只写探针名，粘贴串口/终端里的整段输出，最后按 Ctrl+D 结束输入
test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout

# 方式 B：先保存日志再验（推荐，可重复执行）
# 在仓库根目录执行。若尚未准备 rootfs / 探针镜像，先跑下面两行（只需在镜像变更后重复）：
cargo xtask starry rootfs --arch riscv64
./test-suit/starryos/scripts/prepare-rootfs-with-write_stdout-probe.sh

# 跑 starryos-test，并把终端里的全部输出写入 serial.log（仍可在屏幕上看一遍）
cargo xtask starry test qemu --target riscv64 \
  --test-disk-image target/riscv64gc-unknown-none-elf/rootfs-riscv64-probe.img \
  --shell-init-cmd test-suit/starryos/testcases/probe-write_stdout-0 \
  --timeout 120 \
  2>&1 | tee serial.log

# 用保存的日志与 oracle 期望行比对
test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout serial.log

# 方式 C：显式从 stdin（等价于省略第二个参数）
cat serial.log | test-suit/starryos/scripts/verify-guest-log-oracle.sh write_stdout -
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
