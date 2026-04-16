# StarryOS Bug 修复记录

本文档用于记录 `os/StarryOS` 中已经发现并完成修复的 bug。

## Bug 2：`/proc/[pid]/status` 把 CPU affinity 固定伪装成“只允许 CPU0”

- 类型：语义、正确性、可观测性
- 参考：
  `os/StarryOS-REF/docs/starry_smp_ultimate_integrated.md` 第 18.3、23.4 节
- 修复前相关代码：
  `kernel/src/pseudofs/proc.rs`
- 修复后相关代码：
  `kernel/src/pseudofs/proc.rs`

### 问题现象

修复前，`kernel/src/pseudofs/proc.rs` 中的 `task_status()` 直接把下面两个字段写死：

- `Cpus_allowed:\t1`
- `Cpus_allowed_list:\t0`

这意味着无论线程真实的 affinity 是什么，用户态通过 `/proc/[pid]/status` 看到的永远都是“只能在 CPU0 上运行”。

### 为什么这是 bug

- StarryOS 已经实现了线程 CPU affinity 的内部状态，`sched_getaffinity` 也会读取当前线程的 `cpumask`。
- 但 `/proc/[pid]/status` 却始终返回固定值，导致用户态观测面和内核真实状态不一致。
- 在 SMP 场景下，这会直接误导调试和验证工作：即使线程允许在多个 CPU 上运行，`/proc` 仍然会谎报成单核绑定。

### 修复方式

- 将 `task_status()` 改为读取线程真实的 `cpumask`。
- 新增格式化逻辑，把 affinity 渲染为：
  `Cpus_allowed` 的十六进制位图形式
  `Cpus_allowed_list` 的 CPU 编号/区间形式
- 补充针对 `/proc` 输出的回归测试，覆盖：
  旧实现的固定错误输出
  修复后真实 affinity 输出
  十六进制 mask 的 32-bit 分组顺序
  连续 CPU 区间压缩格式

### Docker 验证

验证使用 `starryos-dev:ubuntu-qemu10.2.1` 镜像完成。由于仓库根 workspace 里目前存在一个与本次修复无关的版本不一致问题，因此验证时只把 `kernel` crate 复制到一个临时 workspace 里执行。

```bash
docker run -d --name starry-proc-validate -v "$PWD":/workspace -w /workspace \
  starryos-dev:ubuntu-qemu10.2.1 sh -lc 'sleep infinity'

docker exec starry-proc-validate sh -lc '
set -e
export PATH=/opt/rustup/toolchains/nightly-2026-02-25-aarch64-unknown-linux-gnu/bin:$PATH
export RUSTUP_TOOLCHAIN=nightly-2026-02-25-aarch64-unknown-linux-gnu
cargo fmt --manifest-path /workspace/os/StarryOS/Cargo.toml --all -- --check
tmp=/tmp/starryos-kernel-tests
rm -rf "$tmp"
mkdir -p "$tmp"
cp -R /workspace/os/StarryOS/kernel "$tmp"/kernel
cp /workspace/os/StarryOS/Cargo.toml "$tmp"/Cargo.toml
sed -i "s/members = [\"starryos\", \"kernel\"]/members = [\"kernel\"]/" "$tmp"/Cargo.toml
sed -i "/starry-kernel = { path = \"kernel\", version = \"0.5.0\" }/d" "$tmp"/Cargo.toml
cd "$tmp"
cargo test -p starry-kernel old_hardcoded_status_lies_about_non_cpu0_affinity -- --nocapture
cargo test -p starry-kernel task_status_reports_real_affinity_instead_of_cpu0_only -- --nocapture
cargo test -p starry-kernel cpus_allowed_hex_matches_actual_affinity_bits -- --nocapture
cargo test -p starry-kernel cpus_allowed_hex_orders_32bit_words_from_high_to_low -- --nocapture
cargo test -p starry-kernel cpus_allowed_list_compacts_contiguous_ranges -- --nocapture
cargo check -p starry-kernel --all-targets
cargo clippy -p starry-kernel --all-targets -- -D warnings
'
```

预期结果：

- `old_hardcoded_status_lies_about_non_cpu0_affinity` 通过，用来证明旧实现的固定输出本身就是错误的。
- `task_status_reports_real_affinity_instead_of_cpu0_only` 通过，用来证明修复后 `/proc/[pid]/status` 会反映真实 affinity。
- 其余三个测试分别验证 mask 的十六进制格式和 CPU 区间格式。

实际 docker 运行结果：

- `test pseudofs::proc::tests::old_hardcoded_status_lies_about_non_cpu0_affinity ... ok`
- `test pseudofs::proc::tests::task_status_reports_real_affinity_instead_of_cpu0_only ... ok`
- `test pseudofs::proc::tests::cpus_allowed_hex_matches_actual_affinity_bits ... ok`
- `test pseudofs::proc::tests::cpus_allowed_hex_orders_32bit_words_from_high_to_low ... ok`
- `test pseudofs::proc::tests::cpus_allowed_list_compacts_contiguous_ranges ... ok`
- `cargo check -p starry-kernel --all-targets` 通过

### 附加检查

- 已在 docker 环境中运行 `cargo fmt --all -- --check`。
- `cargo clippy -p starry-kernel --all-targets -- -D warnings` 当前仍会失败，但失败点是仓库里已有的无关问题，不是本次修复引入的。例如：
  `kernel/src/mm/access.rs`
  `kernel/src/mm/io.rs`
  `kernel/src/task/ops.rs`
  `kernel/src/syscall/signal.rs`
