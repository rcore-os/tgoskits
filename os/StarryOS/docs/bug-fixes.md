# StarryOS Bug 修复记录

本文档用于记录 `os/StarryOS` 中已经发现并完成修复的 bug。

## Bug 1：全局 signal-check 屏蔽位在线程之间串扰

- 类型：并发、语义、正确性
- 参考：
  `os/StarryOS-REF/docs/starry_smp_ultimate_integrated.md` 第 23.6 节
- 修复前相关代码：
  `kernel/src/task/signal.rs`
- 修复后相关代码：
  `kernel/src/task/mod.rs`
  `kernel/src/task/signal.rs`

### 问题现象

`sys_rt_sigreturn()` 的语义是：当前线程从用户态 signal handler 返回后，内核要跳过接下来的一次 signal check，这样同一个线程才能安全地恢复到原本的用户态执行流。

修复前，这个“跳过下一次 signal check”的状态被实现成了一个全局
`static AtomicBool`。在 SMP 场景下，线程 A 设置该标记后，线程 B 可能先一步执行到返回用户态前的检查路径，并把这个一次性标记消费掉。这样就会导致一个线程的 signal-return 状态泄漏到另一个线程。

### 为什么这是 bug

- 这个标记的语义本质上是线程私有，而不是进程私有，更不是系统全局。
- 全局一次性标记会导致线程间相互干扰。
- 在 SMP 下，错误的线程可能跳过 signal delivery，而真正应该跳过检查的线程却没有跳过。

### 修复方式

- 在 `kernel/src/task/mod.rs` 中引入专用的一次性状态
  `NextSignalCheckBlock`。
- 把该状态存进每个 `Thread` 对象，而不是放在全局静态变量里。
- 修改 `kernel/src/task/signal.rs` 中的 `block_next_signal()` /
  `unblock_next_signal()`，让它们只操作当前线程自己的私有状态，不再访问全局原子变量。

### Docker 验证

验证使用 `starryos-dev:ubuntu-qemu10.2.1` 镜像完成。由于仓库根 workspace 里目前存在一个与本次修复无关的版本不一致问题，因此验证时只把 `kernel` crate 复制到一个临时 workspace 里执行。

```bash
docker run --rm -v "$PWD":/workspace starryos-dev:ubuntu-qemu10.2.1 sh -lc '
set -e
tmp=/tmp/starryos-kernel-tests
rm -rf "$tmp"
mkdir -p "$tmp"
cp -R /workspace/os/StarryOS/kernel "$tmp"/kernel
cp /workspace/os/StarryOS/Cargo.toml "$tmp"/Cargo.toml
sed -i "s/members = \[\"starryos\", \"kernel\"\]/members = [\"kernel\"]/" "$tmp"/Cargo.toml
sed -i "/starry-kernel = { path = \"kernel\", version = \"0.5.0\" }/d" "$tmp"/Cargo.toml
cd "$tmp"
export PATH=/opt/rustup/toolchains/nightly-2026-02-25-aarch64-unknown-linux-gnu/bin:$PATH
export RUSTUP_TOOLCHAIN=nightly-2026-02-25-aarch64-unknown-linux-gnu
cargo test -p starry-kernel old_global_signal_check_block_leaks_between_threads -- --nocapture
cargo test -p starry-kernel per_thread_signal_check_block_is_isolated -- --nocapture
'
```

预期结果：

- `old_global_signal_check_block_leaks_between_threads` 通过，用来复现旧实现的 bug，也就是证明旧的全局标记确实会跨“逻辑线程”泄漏。
- `per_thread_signal_check_block_is_isolated` 通过，用来证明修复后的每线程私有状态不会再发生串扰。

实际 docker 运行结果：

- `test task::tests::old_global_signal_check_block_leaks_between_threads ... ok`
- `test task::tests::per_thread_signal_check_block_is_isolated ... ok`

### 附加检查

- 已在 docker 环境中运行 `cargo fmt --all`。
- 在临时 kernel workspace 中，`cargo check -p starry-kernel --all-targets` 通过。
- `cargo clippy -p starry-kernel --all-targets -- -D warnings` 当前仍会失败，但失败点是仓库里已有的无关问题，不是本次修复引入的。例如：
  `kernel/src/mm/access.rs`
  `kernel/src/mm/io.rs`
  `kernel/src/task/ops.rs`
  `kernel/src/syscall/signal.rs`

## Bug 4：`/proc/stat` 缺失导致 `busybox iostat` 无法工作

- 类型：语义、正确性、兼容性
- 关联用户态程序：
  `busybox iostat`
- 修复前相关代码：
  `kernel/src/pseudofs/proc.rs`
- 修复后相关代码：
  `kernel/src/pseudofs/proc.rs`
  `test-suit/starryos/normal/qemu-smp1/busybox-iostat`

### 问题现象

在 StarryOS 中执行 `busybox iostat 1 1` 时，程序不会输出 CPU/磁盘统计信息，而是直接报错退出：

`iostat: can't open '/proc/stat': No such file or directory`

这说明用户态工具已经正常启动，但在读取 Linux 兼容接口时立刻失败。进一步检查还可以发现，StarryOS 的 `/proc` 根目录虽然已经实现了 `meminfo`、`mounts`、`interrupts` 等节点，但仍然缺少 `iostat` 依赖的几个基础统计文件：

- `/proc/stat`
- `/proc/diskstats`
- `/proc/uptime`

由于这些节点不存在，`busybox iostat` 在最开始读取 CPU 统计信息时就直接退出，后续逻辑完全没有机会继续执行。

### 为什么这是 bug

- `busybox iostat` 是典型的 Linux 用户态系统观测工具，它不是依赖某个私有驱动接口，而是依赖标准 procfs 统计节点。
- StarryOS 已经提供了 `/proc` 文件系统，并且目标就是模拟 Linux 兼容环境；在这种前提下，缺失 `iostat` 所依赖的核心节点会导致兼容性断裂。
- 这不是“功能还没做”的抽象缺口，而是一个可以被稳定复现的用户态失败：命令启动后立刻因为缺文件报错。
- 即使当前系统里没有完整的磁盘吞吐统计数据，也至少应该提供最基本、可读取、格式正确的 procfs 节点，让标准工具能够运行并输出合理的结果，而不是直接失败。

### 修复方式

本次修复没有去扩展复杂的块设备统计逻辑，而是采用“最小兼容补全”的方式，先把 `busybox iostat` 真正依赖的 procfs 接口补上：

- 在 `kernel/src/pseudofs/proc.rs` 中新增 `/proc/stat`：
  - 提供总 CPU 行 `cpu ...`
  - 提供每个逻辑 CPU 的 `cpuN ...` 行
  - 补充 `intr`、`ctxt`、`btime`、`processes`、`procs_running`、`procs_blocked`、`softirq` 等基础字段
- 新增 `/proc/uptime`：
  - 基于系统单调时钟 `monotonic_time_nanos()` 动态生成运行时间
  - 以 Linux 常见的“秒.百分秒”格式输出
- 新增 `/proc/diskstats`：
  - 当前先提供一个可打开、可读取的空统计文件
  - 这样 `iostat` 不会因为节点缺失而中止，后续即使没有设备统计，也可以先完成 CPU 部分输出
- 新增若干单元测试，验证：
  - `/proc/stat` 的 CPU 行格式正确
  - `/proc/uptime` 的时间格式正确
  - `/proc/diskstats` 至少稳定存在
  - CPU 空闲 tick 计算遵循 Linux 常见的 `USER_HZ=100`

这个修复策略的核心不是伪造复杂数据，而是先补齐 Linux 用户态真正依赖的接口边界，让标准工具从“直接失败”变成“能够正常工作并输出可解析结果”。

### Docker 验证

验证使用 `starryos-dev:ubuntu-qemu10.2.1` 镜像完成。

#### 1. 修复前，bug 确实存在

在修复前的临时 probe case 中，QEMU 内执行 `busybox iostat 1 1` 的实际输出为：

`iostat: can't open '/proc/stat': No such file or directory`

这一步用来证明：问题不是测例写错，也不是 `busybox` 自身不可用，而是 StarryOS 的 procfs 缺少必要节点，导致 `iostat` 无法运行。

#### 2. 修复后，专用回归测例通过

新增测例：

- `test-suit/starryos/normal/qemu-smp1/busybox-iostat/qemu-x86_64.toml`
- `test-suit/starryos/normal/qemu-smp1/busybox-iostat/sh/busybox-iostat.sh`

验证命令：

```bash
docker run --rm -v "$PWD":/workspace -w /workspace \
  starryos-dev:ubuntu-qemu10.2.1 \
  bash -lc '
    set -e
    export RUSTUP_TOOLCHAIN=nightly-2026-04-27
    cargo xtask starry test qemu --target x86_64-unknown-none -c busybox-iostat
  '
```

修复后的 QEMU 关键输出：

```text
Linux 10.0.0 (starry)  05/09/26  _x86_64_  (1 CPU)

avg-cpu:  %user   %nice %system %iowait  %steal   %idle
           0.00    0.00    0.00    0.00    0.00  100.00

Device:            tps   Blk_read/s   Blk_wrtn/s   Blk_read   Blk_wrtn
TEST PASSED
```

这说明修复后 `busybox iostat` 已经可以正常读取 procfs 并完成输出，不再因为缺少 `/proc/stat` 而直接失败。

### 附加检查

- 已运行 `cargo fmt --manifest-path Cargo.toml --all`
- 已在 docker 中运行 `cargo check -p starry-kernel --all-targets`，通过
- 已在 docker 中运行 `cargo xtask clippy --package starry-kernel`，7 个检查全部通过
