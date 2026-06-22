# ArceOS 开发指南

ArceOS 既是可以单独运行的模块化 Unikernel，也是 StarryOS 与 Axvisor 共享的基础能力层。本文档面向在 TGOSKits 工作区内进行 ArceOS 相关开发的场景，覆盖开发环境、模块开发规范、应用与平台开发、测试策略、调试技巧和跨架构验证。

> 仓库布局、架构分层和模块体系见 [ArceOS 架构](/docs/architecture/arceos)。
> 最短命令和快速启动见 [快速开始](/docs/quickstart/overview)。
> 构建系统总览见 [构建与运行](/docs/build/overview)。

---

## 1. 开发环境

### 1.1 工具链

TGOSKits 工作区根目录的 `rust-toolchain.toml` 已锁定统一工具链：

| 配置项 | 值 |
|--------|-----|
| channel | `nightly-2026-04-27` |
| profile | `minimal` |
| components | `rust-src`, `llvm-tools`, `rustfmt`, `clippy` |
| targets | `x86_64-unknown-none`, `riscv64gc-unknown-none-elf`, `aarch64-unknown-none-softfloat`, `loongarch64-unknown-none-softfloat` |

进入工作区后 `rustup` 会自动切换到该工具链，无需手动配置。

### 1.2 QEMU

ArceOS 开发和测试依赖 QEMU system emulator：

| 架构 | QEMU 包名 | 验证命令 |
|------|-----------|---------|
| aarch64 | `qemu-system-aarch64` | `qemu-system-aarch64 --version` |
| riscv64 | `qemu-system-riscv64` | `qemu-system-riscv64 --version` |
| x86_64 | `qemu-system-x86_64` | `qemu-system-x86_64 --version` |
| loongarch64 | `qemu-system-loongarch64` | `qemu-system-loongarch64 --version` |

推荐版本 ≥ 10.2.1。Debian/Ubuntu 安装示例：

```bash
sudo apt install qemu-system-arm qemu-system-riscv64 qemu-system-x86
```

### 1.3 交叉编译工具链（可选）

大部分场景下 `cargo` + `rust-src` 即可完成 `no_std` 交叉编译，无需额外交叉工具链。仅当模块依赖 C 代码或需要链接外部 `.a` 时，才需安装对应的 `gcc` 交叉编译器。

---

## 2. 目录结构总览

```
os/arceos/
├── modules/          # 内核模块
│   ├── axhal/        # 硬件抽象层
│   ├── axtask/       # 任务/线程管理 + 调度器
│   ├── axalloc/      # 内存分配器
│   ├── axdriver/     # 统一设备驱动框架
│   ├── axfs/         # 文件系统（legacy）
│   ├── axfs-ng/      # 文件系统（next-gen, ext4/fat）
│   ├── axlog/        # 多级日志
│   ├── axsync/       # 同步原语
│   ├── axmm/         # 页表/内存管理
│   ├── axdisplay/    # 图形显示
│   ├── axdma/        # DMA 支持
│   ├── axinput/      # 输入设备
│   ├── axipi/        # 核间中断
│   ├── axruntime/    # 运行时初始化，调用 main()
│   └── axconfig/     # 编译时配置生成
├── api/              # 对外 API 层
│   ├── axfeat/       # 顶层 feature 聚合（单一真相源）
│   ├── arceos_api/   # 公共 API 和类型
│   └── arceos_posix_api/  # POSIX 兼容 API
├── ulib/             # 用户侧库
│   ├── axstd/        # Rust std 风格接口
│   └── axlibc/       # C libc 接口

net/
└── ax-net/           # 统一网络栈（TCP/UDP/raw/Unix/vsock/DNS/DHCP）

apps/arceos/
├── helloworld/
├── httpserver/
├── httpclient/
├── io_test/
├── thread_test/
├── tokio_test/
├── arce_agent/
├── shell/
└── helloworld-myplat/
```

---

## 3. 模块开发

### 3.1 模块标准结构

以 `axtask` 为代表的典型模块结构：

```
modules/axtask/
├── Cargo.toml        # features + 依赖
└── src/
    ├── lib.rs        # 模块根，条件编译，re-exports
    ├── api.rs        # 公共 API 函数
    ├── task.rs       # Task 结构体
    ├── run_queue/    # 调度器 run queue 实现
    └── wait_queue.rs
```

关键约定：

- **`lib.rs`**：使用 `cfg_if!` 和 `#[cfg(feature = "...")]` 进行条件编译，通过 `pub use` 向外暴露公共 API
- **`api.rs`**：存放面向应用的公共函数，如 `spawn()`, `sleep()`, `yield_now()`
- **init 函数**：模块暴露 `init_*()` 函数，由 `axruntime::rust_main()` 在启动时根据 feature 配置调用

典型的 init 调用链（`axruntime` 中）：

```rust
// axruntime/src/lib.rs (简化)
pub unsafe fn rust_main() {
    ax_log::init();
    ax_hal::platform_init();
    ax_alloc::init();
    #[cfg(feature = "paging")]
    ax_mm::init();
    #[cfg(feature = "multitask")]
    ax_task::init_scheduler();
    #[cfg(feature = "fs-ng")]
    ax_fs_ng::init_filesystems(/* ... */);
    #[cfg(feature = "net")]
    ax_net::init_network(/* ... */);
    // ...
    main();
}
```

### 3.2 开发一个新模块

假设要添加 `axmymod` 模块，步骤如下：

**1) 创建目录和文件**

```
os/arceos/modules/axmymod/
├── Cargo.toml
└── src/
    ├── lib.rs
    └── api.rs
```

**2) 编写 `Cargo.toml`**

```toml
[package]
name = "ax-mymod"
version.workspace = true
edition.workspace = true

[dependencies]
ax-feat = { path = "../../api/axfeat" }
log = "0.4"

[features]
default = []
myfeature = []
```

**3) 编写 `lib.rs`**

```rust
#![no_std]

extern crate log;

mod api;

pub use api::*;
```

**4) 编写 `api.rs`，暴露 init 函数和公共 API**

```rust
use log::info;

pub fn init() {
    info!("axmymod initialized.");
}

pub fn do_something() -> i32 {
    42
}
```

**5) 在 `axruntime` 中接入 init 调用**

在 `os/arceos/modules/axruntime/src/lib.rs` 的 `rust_main()` 中添加：

```rust
#[cfg(feature = "mymod")]
ax_mymod::init();
```

**6) 在 `axfeat` 中注册 feature**

在 `os/arceos/api/axfeat/Cargo.toml` 中添加：

```toml
[features]
mymod = ["dep:ax-mymod", "ax-runtime/mymod"]

[dependencies]
ax-mymod = { path = "../../modules/axmymod", optional = true }
```

**7) 验证**

```bash
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64 --features mymod
```

### 3.3 Feature 驱动编译

ArceOS 的核心设计是 **feature 聚合**：应用在 `Cargo.toml` 中声明需要的 feature，`axfeat` 将它们传播到对应模块。

`axfeat` 中的 feature 定义示例（简化）：

```toml
[features]
# CPU
smp = ["alloc", "ax-hal/smp", "ax-runtime/smp", "ax-task?/smp"]
fp-simd = ["ax-hal/fp-simd"]

# 内存
alloc = ["ax-alloc", "ax-runtime/alloc"]
paging = ["alloc", "ax-hal/paging", "ax-runtime/paging"]

# 任务
multitask = ["alloc", "ax-task/multitask", "ax-sync/multitask", "ax-runtime/multitask"]
sched-fifo = ["ax-task/sched-fifo"]
sched-rr = ["ax-task/sched-rr", "irq"]
sched-cfs = ["ax-task/sched-cfs", "irq"]

# 上层协议栈
fs = ["alloc", "paging", "ax-driver/virtio-blk", "dep:ax-fs", "ax-runtime/fs"]
net = ["alloc", "paging", "ax-driver/virtio-net", "dep:ax-net", "ax-runtime/net"]
```

这意味着：

- 启用 `smp` 会自动启用 `alloc` 并传播到 `ax-hal`、`ax-runtime`、`ax-task`
- 启用 `net` 会自动拉起 alloc + paging + virtio-net 驱动 + ax-net 模块
- 应用只需关心自身需要的功能，不需要了解底层模块的依赖图

### 3.4 修改已有模块

修改已有模块时的推荐流程：

| 改动类型 | 验证命令 | 扩展验证 |
|----------|---------|---------|
| 基础 crate（`axerrno`, `kspin`, `page_table_multiarch`） | `cargo test -p <crate>` | `cargo xtask arceos qemu --package arceos-helloworld --arch riscv64` |
| HAL（`axhal`） | `cargo xtask arceos qemu --package arceos-helloworld --arch aarch64` | 多架构验证 |
| 调度器（`axtask`） | `cargo xtask arceos qemu --package arceos-helloworld --arch riscv64` | `cargo xtask arceos test qemu --target riscv64gc-unknown-none-elf` |
| 网络（`axnet` / `axnet`） | `cargo xtask arceos qemu --package arceos-httpserver --arch aarch64 --net` | 检查 TCP 连接和吞吐 |
| 文件系统（`axfs` / `axfs-ng`） | `cargo xtask arceos qemu --package arceos-shell --arch aarch64 --blk` | 检查文件读写 |
| 驱动（`axdriver`） | `cargo xtask arceos qemu --package arceos-helloworld --arch aarch64` | 启用对应设备 `--blk` / `--net` |

---

## 4. 应用开发

### 4.1 新增 Rust 示例应用

**1) 创建目录和文件**

```
apps/arceos/myapp/
├── Cargo.toml
└── src/
    └── main.rs
```

**2) `Cargo.toml`**

```toml
[package]
name = "arceos-myapp"
version = "0.1.0"
edition.workspace = true

[features]
default = []
arceos = ["dep:ax-std"]

[dependencies]
ax-std = { workspace = true, optional = true }

[package.metadata.axstd]
features = ["log-level-debug"]
```

**3) `src/main.rs`**

```rust
#[cfg(feature = "arceos")]
use ax_std as _;

fn main() {
    println!("Hello from myapp!");
}
```

> `arceos` feature 由 axbuild 的 std-aware 构建流程注入；应用代码保持普通 Rust `std` app 风格。

**4) 验证**

```bash
cargo xtask arceos qemu --package arceos-myapp --arch aarch64
```

### 4.2 使用 `axstd` 的 `std` 风格 API

对于复杂应用（如 `httpserver`），`axstd` 提供了接近 Rust `std` 的 API：

```rust
use std::{io::Read, net::TcpListener, thread, time::Duration};

fn main() {
    let listener = TcpListener::bind("0.0.0.0:8080").unwrap();
    loop {
        let mut stream = listener.accept().unwrap().0;
        let mut buf = [0u8; 1024];
        stream.read(&mut buf).ok();
        // 处理请求...
        thread::sleep(Duration::from_millis(100));
    }
}
```

对应 `Cargo.toml` 需要通过 `arceos` feature 为 axbuild 注入网络能力：

```toml
[features]
default = []
arceos = ["dep:ax-std", "ax-std/net"]

[dependencies]
ax-std = { workspace = true, optional = true }
```

### 4.3 C 应用覆盖

C 应用覆盖由 `test-suit/arceos/c` 维护；`apps/arceos` 只保留 Rust std app。

### 4.4 Feature 与应用对应关系

| 功能需求 | 需要启用的 feature | 示例命令 |
|----------|-------------------|---------|
| 最小运行 | `ax-std` | `--package arceos-helloworld` |
| 多任务 | `multitask` | 在应用 Cargo.toml 中启用 |
| 网络 | `net` | `--package arceos-httpserver` |
| 文件系统 | `fs` 或 `fs-ng` | `--package arceos-shell` |
| 多核 | `smp` | `--arch aarch64` + `SMP=4` |
| PCI 设备 | `bus-pci` | 默认 |
| MMIO 设备 | `bus-mmio` | `--features bus-mmio` |

---

## 5. 平台开发

### 5.1 平台 crate 结构

以 `ax-plat-riscv64-sg2002` 为例：

```
platforms/ax-plat-riscv64-sg2002/
├── Cargo.toml
├── axconfig.toml     # 平台配置（内存布局、SMP 数等）
├── build.rs          # 构建脚本
└── src/
    └── lib.rs        # 实现 console/time/irq 等平台接口
```

平台 crate 需要实现的接口由 `axconfig` 和 `axhal` 定义，包括：

- **console**：`write_text_bytes()` — 字符输出
- **time**：`current_time()`, `set_oneshot_timer()` — 时钟
- **irq**：`set_extern_irq_handler()`, `enable_irq()` — 中断管理

### 5.2 平台目录

| 目录 | 内容 |
|------|------|
| `platforms/` | 工作区内 `ax-plat-*` 平台 crate |
| `platforms/axplat-dyn/` | 动态平台加载（设备树驱动） |

已有平台：

| 平台 | 架构 | 目标硬件 |
|------|------|---------|
| `ax-plat-riscv64-sg2002` | riscv64 | SG2002 板级平台 |
| `axplat-loongarch64-qemu-virt` | loongarch64 | QEMU virt |

AArch64、RISC-V QEMU 和 x86_64 QEMU 默认平台由 `axplat-dyn` 通过设备树/运行时信息加载，不再维护仓库内静态平台 crate。

### 5.3 添加新平台

1. 在 `platforms/` 下创建新 crate
2. 实现 `axhal` 要求的平台接口
3. 编写 `axconfig.toml` 配置内存布局和硬件参数
4. 在根 `Cargo.toml` 中注册为 workspace member
5. 验证：

```bash
cargo xtask arceos qemu --package arceos-helloworld --arch <arch> --platform <platform-name>
```

---

## 6. 测试

### 6.1 单元测试

对于支持 `std` 测试的基础 crate，直接运行：

```bash
cargo test -p ax-errno
cargo test -p ax-kspin
```

### 6.2 test-suit 集成测试

ArceOS 的集成测试位于 `test-suit/arceos/`，按功能分类：

| 类别 | 测试项 |
|------|--------|
| `task/` | affinity, ipi, irq, lockdep, parallel, priority, sleep, tls, wait_queue, yield |
| `net/` | httpclient |
| `fs/` | shell |
| `display/` | 显示测试 |
| `memtest/` | 内存测试 |
| `exception/` | 异常处理 |

C 测试位于 `test-suit/arceos/c/`：helloworld, httpclient, memtest, pthread。

### 6.3 测试配置格式

每个测试由两个 TOML 文件定义：

**`build-<target>.toml`** — 构建配置：

```toml
features = ["ax-std"]
log = "Error"
max_cpu_num = 4

[env]
AX_IP = "10.0.2.15"
AX_GW = "10.0.2.2"
```

**`qemu-<arch>.toml`** — QEMU 运行配置：

```toml
args = ["-machine", "virt", "-cpu", "cortex-a72", "-m", "128M", "-smp", "4"]
uefi = false
to_bin = true
success_regex = ["All tests passed!"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b"]
```

关键字段说明：

| 字段 | 说明 |
|------|------|
| `features` | 启用的 ArceOS feature 列表 |
| `log` | 日志级别（error/warn/info/debug/trace） |
| `max_cpu_num` | 最大 CPU 数 |
| `plat_dyn` | 是否使用动态平台 |
| `args` | QEMU 启动参数 |
| `success_regex` | 匹配成功的正则 |
| `fail_regex` | 匹配失败的正则 |

### 6.4 运行测试

```bash
# 通过 xtask 运行 ArceOS 全部 QEMU 测试
cargo xtask arceos test qemu --target riscv64gc-unknown-none-elf

# 指定架构运行
cargo xtask arceos test qemu --target aarch64-unknown-none-softfloat
```

### 6.5 添加新测试用例

1. 在 `test-suit/arceos/rust/<category>/` 或 `test-suit/arceos/c/` 下创建测试项目
2. 编写 `build-<target>.toml` 和 `qemu-<arch>.toml`
3. 确认 `success_regex` 和 `fail_regex` 能正确匹配输出
4. 通过 `cargo xtask arceos test qemu` 验证

---

## 7. 日志与调试

### 7.1 日志系统

ArceOS 使用 `axlog` 模块提供 5 级日志：

```rust
use log::{error, warn, info, debug, trace};

error!("致命错误");
warn!("警告");
info!("信息");
debug!("调试信息");
trace!("追踪信息");
```

设置日志级别：

```bash
# 使用 Makefile
cd os/arceos
make A=examples/helloworld ARCH=riscv64 LOG=debug run
```

日志级别从编译时和运行时两个层面控制：

- **编译时**：`LOG=` 或 `AX_LOG` 环境变量，决定哪些日志宏会被编译进二进制
- **运行时**：`ax_log::set_max_level()` 进一步过滤

`ax_print!` / `ax_println!` 宏用于无条件输出（不受日志级别影响）。

### 7.2 GDB 调试

```bash
cd os/arceos
make A=examples/helloworld ARCH=riscv64 debug
```

此命令会启动 QEMU 并暂停等待 GDB 连接。然后在另一个终端：

```bash
# aarch64
aarch64-none-elf-gdb target/aarch64-unknown-none-softfloat/release/helloworld
# riscv64
riscv64-unknown-elf-gdb target/riscv64gc-unknown-none-elf/release/helloworld
```

GDB 连接：

```
(gdb) target remote :1234
(gdb) break rust_main
(gdb) continue
```

### 7.3 Makefile 变量速查

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `ARCH` | `x86_64` | 目标架构 |
| `A` / `APP` | `examples/helloworld` | 应用路径 |
| `MODE` | `release` | release 或 debug |
| `LOG` | `warn` | 日志级别 |
| `SMP` | 平台默认 | CPU 数量 |
| `FEATURES` | — | 额外 feature |
| `BLK` | `n` | 启用 virtio-blk |
| `NET` | `n` | 启用 virtio-net |
| `BUS` | `pci` | `pci` 或 `mmio` |
| `MEM` | `128M` | 内存大小 |
| `IP` / `GW` | `10.0.2.15` / `10.0.2.2` | 网络配置 |

### 7.4 xtask vs Makefile

| 方式 | 适用场景 |
|------|---------|
| `cargo xtask arceos` | 根目录入口，CI 一致，跨系统集成验证 |
| `os/arceos/Makefile` | 快速调试，细粒度控制参数，单模块开发 |

建议：

- 日常开发、调参数 → 使用 Makefile
- 提交前验证、CI 对齐、集成测试 → 使用 xtask

---

## 8. 多架构验证

ArceOS 支持 4 种架构。改动核心模块后建议在至少 2 个架构上验证：

| 架构 | 编译目标 | QEMU 启动命令 |
|------|---------|-------------|
| aarch64 | `aarch64-unknown-none-softfloat` | `cargo xtask arceos qemu --package arceos-helloworld --arch aarch64` |
| riscv64 | `riscv64gc-unknown-none-elf` | `cargo xtask arceos qemu --package arceos-helloworld --arch riscv64` |
| x86_64 | `x86_64-unknown-none` | `cargo xtask arceos qemu --package arceos-helloworld --arch x86_64` |
| loongarch64 | `loongarch64-unknown-none-softfloat` | `cargo xtask arceos qemu --package arceos-helloworld --arch loongarch64` |

推荐的最小验证矩阵：

```bash
# 改动基础 crate
cargo test -p <crate>
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64
cargo xtask arceos qemu --package arceos-helloworld --arch riscv64

# 改动驱动/设备
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64
cargo xtask arceos qemu --package arceos-helloworld --arch riscv64
cargo xtask arceos qemu --package arceos-helloworld --arch x86_64

# 改动调度器/多核
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64  # SMP=4
cargo xtask arceos qemu --package arceos-helloworld --arch riscv64  # SMP=4
```

---

## 9. 常见问题

### Q: QEMU 启动后无输出？

- 确认 `--arch` 参数与 QEMU 二进制匹配
- 尝试提高日志级别 `LOG=info`
- 检查是否遗漏了必要的 feature

### Q: 编译报 `linker 'rust-lld' not found`？

确认工具链已正确安装：`rustup show` 应显示 `nightly-2026-04-27` 且包含 `rust-src` 组件。

### Q: 网络/块设备示例启动失败？

确认 QEMU 参数中包含对应设备：

```bash
# 网络
cargo xtask arceos qemu --package arceos-httpserver --arch aarch64 --net

# 块设备
cargo xtask arceos qemu --package arceos-shell --arch aarch64 --blk
```

### Q: 如何确认改动不影响 StarryOS / Axvisor？

如果改动位于共享组件或 ArceOS 模块层：

```bash
# ArceOS 验证
cargo xtask arceos qemu --package arceos-helloworld --arch aarch64

# StarryOS 验证
cargo xtask starry qemu --arch riscv64

# Axvisor 验证
cargo xtask axvisor defconfig qemu-aarch64
cargo xtask axvisor build --config os/axvisor/.build.toml
```

---

## 10. 推荐阅读

- [ArceOS 架构](/docs/architecture/arceos): 分层、feature 装配、模块体系
- [组件开发指南](/docs/development/components): 共享依赖如何接到三个系统
- [构建与运行](/docs/build/overview): xtask、Makefile 与 workspace 边界
- [StarryOS 开发指南](/docs/development/starryos): 改动波及 StarryOS 时
- [Axvisor 开发指南](/docs/development/axvisor): 改动波及 Axvisor 时
