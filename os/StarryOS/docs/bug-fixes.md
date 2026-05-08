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
