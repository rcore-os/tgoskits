# Panic Backtrace

> 当前仅对 **aarch64** 架构测试通过。x86_64、riscv64、loongarch64 的 frame pointer 约定不同，启用后需额外验证。

## 用法

backtrace 捕获是昂贵的运行时操作，默认关闭以避免 release 构建的性能开销。

如需启用，在 board config 中取消注释 `"backtrace"` feature 和 `[env]` 段：

```toml
features = [
    "ax-driver/virtio-blk",
    "fs",
    # Capturing a backtrace is an expensive runtime operation.
    # Enable "backtrace" and [env] BACKTRACE = "y" below to
    # activate panic backtrace. Disabled by default for release.
    "backtrace",
]
# ...
# Set BACKTRACE=y together with the "backtrace" feature above to
# add -Cforce-frame-pointers=yes via axbuild.
[env]
BACKTRACE = "y"
```

如需关闭，注释掉 `"backtrace"` feature 和 `[env]` 段即可（默认状态）。不配 `"backtrace"` 时，panic hook 不编译，axbacktrace 的 `alloc` feature 不开启，`capture()` 返回 `Disabled`。

## 效果

panic 时输出：

```
panicked at os/axvisor/src/main.rs:70:5:
backtrace verification
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=true dwarf=false
BT 0 ip=0xffff8000514c fp=0xff0040742bc0
BT 1 ip=0xffff801dbdd0 fp=0xff0040742c30
...
BT 12 ip=0xffff8013b8d8 fp=0xff0040742fe0
BACKTRACE_END
```

裸地址需 host 端离线符号化后才能阅读。

## 翻译（符号化）

崩溃是突发的，不一定提前 pipe 了输出。三种场景：

### 场景一：还没崩，做好预防

运行时就存日志：

```bash
# 输出同时保存到文件，崩了也不怕
cargo xtask axvisor qemu --config qemu-aarch64 2>&1 | tee qemu.log

# 崩完后翻译
cargo xtask backtrace symbolize \
    --elf target/aarch64-unknown-linux-musl/release/axvisor \
    --log qemu.log
```

### 场景二：已经崩了，输出还在终端里

从终端上把 `BACKTRACE_BEGIN` 到 `BACKTRACE_END` 这一段复制下来，贴到文件里：

```bash
cat > bt.log    # 然后粘贴，Ctrl+D 结束
# 或
vim bt.log      # 贴进去保存

# 翻译
cargo xtask backtrace symbolize \
    --elf target/aarch64-unknown-linux-musl/release/axvisor \
    --log bt.log
```

或者直接贴到命令里：

```bash
cargo xtask backtrace symbolize \
    --elf target/aarch64-unknown-linux-musl/release/axvisor \
<<'EOF'
BACKTRACE_BEGIN kind=panic arch=aarch64 alloc=true dwarf=false
BT 0 ip=0xffff80004cc4 fp=0xff0040742bc0
...
BACKTRACE_END
EOF
```

### 场景三：Pipe 一条龙

适用于还来得及重跑的场景：

```bash
cargo xtask axvisor qemu --config qemu-aarch64 2>&1 |
  cargo xtask backtrace symbolize \
    --elf target/aarch64-unknown-linux-musl/release/axvisor
```

输出示例：

```
BT 0  axvisor::init_panic_hook::{closure#1}
BT 1  std::panicking::panic_with_hook
BT 2  std::panicking::panic_handler::{closure#0}
BT 3  __rustc::rust_begin_unwind
BT 4  core::panicking::panic_fmt
BT 5  axvisor::main
BT 6  std::rt::lang_start::<()>::{closure#0}
BT 7  std::rt::lang_start_internal
BT 8  main
...
```

## 实现架构

```
board config: features = ["backtrace"]
  → cargo build --features backtrace
    → Cargo.toml: backtrace feature (ax-std/backtrace + dep:axbacktrace)
    → main.rs: #[cfg(feature = "backtrace")] 注册 panic hook

board config: [env] BACKTRACE = "y"
  → axbuild: add -Cforce-frame-pointers=yes
  → 保证 release + panic=abort 下帧指针有效
```

## 与 axruntime #[panic_handler] 的关系

axvisor 是 std 构建（`aarch64-unknown-linux-musl`），std 的 panic handler 覆盖了 `axruntime::lang_items` 的 `#[panic_handler]`。所以 panic backtrace 不走 axruntime 路径，而是通过 `std::panic::set_hook()` 注册的 hook 触发。
