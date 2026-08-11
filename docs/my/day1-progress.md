# TGOSKits Day1 进度记录：环境与三条启动链路

> 日期：2026-08-07
> 范围：TGOSKits 架构理解、Rust/QEMU 环境、AxVisor、ArceOS、Linux、StarryOS 启动验证

---

## 目录

- [1. Day1 结论](#1-day1-结论)
- [2. Day1 目标](#2-day1-目标)
- [3. 项目架构认识](#3-项目架构认识)
- [4. 环境准备](#4-环境准备)
- [5. 已完成的验证](#5-已完成的验证)
- [6. 遇到的问题与解决过程](#6-遇到的问题与解决过程)
- [7. 关键证据](#7-关键证据)
- [8. Day1 验收表](#8-day1-验收表)
- [9. 后续每日记录模板](#9-后续每日记录模板)

## 1. Day1 结论

Day1 的核心目标已经完成。

最终打通了两条路径：

```text
路径 A：QEMU → AxVisor → ArceOS
                    └→ Linux

路径 B：QEMU → StarryOS
```

已经确认：

- AxVisor AArch64 可以启动。
- AxVisor 可以启动 ArceOS 客户机。
- AxVisor 可以启动 Linux 客户机，并挂载 NVMe 根文件系统进入 shell。
- StarryOS AArch64 可以直接启动，并进入：

```text
root@starry:/root #
```

Day1 的启动验证没有修改项目源代码；本文件是验证结束后新增的进度记录。StarryOS 的最初失败来自本地 `RUSTFLAGS` 环境变量覆盖，而不是项目链接脚本本身损坏。

## 2. Day1 目标

Day1 不是实现新功能，而是建立后续开发所需的可运行基线：

1. 看懂 TGOSKits 中 AxVisor、ArceOS、StarryOS 的层次关系。
2. 安装并验证 Rust、AArch64 工具链和 QEMU。
3. 使用项目推荐的 `cargo xtask` 命令，而不是脱离项目流程手动拼接参数。
4. 启动 AxVisor，并验证至少一个 ArceOS 客户机和一个 Linux 客户机。
5. 独立启动 StarryOS，确认其不是只有构建产物，而是真正进入内核和用户 shell。

验收标准是“看到可重复的启动证据”，不是仅仅看到 Cargo 编译成功。

## 3. 项目架构认识

### 3.1 三个系统不在同一层

| 系统 | 所在层次 | 主要职责 | Day1 中的关系 |
| --- | --- | --- | --- |
| AxVisor | Hypervisor/虚拟机监控层 | 管理虚拟机、CPU、内存和虚拟设备，启动 guest | 作为宿主，启动 ArceOS 和 Linux |
| ArceOS | Unikernel/组件化内核层 | 提供轻量内核、驱动和运行时组件 | 作为 AxVisor guest 验证 |
| StarryOS | 面向 Linux 应用兼容的多进程 OS 层 | 提供进程、syscall、VFS、Linux 用户态兼容能力 | 直接在 QEMU 上启动 |

因此，StarryOS 与 ArceOS 可以比较为“操作系统/unikernel”层，但 AxVisor 是承载 guest 的虚拟化层，不是与 StarryOS 同级的系统。

### 3.2 TGOSKits 的含义

TGOSKits 是整个工作区和工具链的项目名称，不是某一个单独的操作系统。它把以下内容放在同一个仓库中：

- 可复用组件：`components/`、`memory/`、`virtualization/`。
- 可移植驱动：`drivers/`。
- ArceOS：`os/arceos/`。
- StarryOS：`os/StarryOS/`。
- AxVisor：`os/axvisor/`。
- 统一构建、运行和测试工具：`cargo xtask`、`scripts/axbuild/`、`test-suit/`。

### 3.3 四个架构

项目主要维护四个目标架构：

| 架构 | 常见 target | Day1 使用情况 |
| --- | --- | --- |
| AArch64 | `aarch64-unknown-none-softfloat` | 主验证架构，AxVisor、Linux、StarryOS 均验证 |
| RISC-V 64 | `riscv64gc-unknown-none-elf` 等 | 阅读和后续测试范围 |
| x86_64 | `x86_64-unknown-none` 等 | 阅读和后续测试范围 |
| LoongArch 64 | `loongarch64-unknown-none-softfloat` 等 | 阅读和后续测试范围 |

Day1 选择 AArch64，是因为 QEMU `virt`、设备模型和项目文档的第一条成功路径都最完整。

### 3.4 Cargo、xtask 和 config ls

Cargo 是 Rust 的包管理和构建工具，负责读取 `Cargo.toml`、解析依赖、调用 rustc、组织 target 和 feature。TGOSKits 在 Cargo 之上提供了统一的 xtask 命令：

```text
cargo xtask arceos ...
cargo xtask starry ...
cargo xtask axvisor ...
```

例如：

```bash
cargo xtask axvisor config ls
```

它只列出 AxVisor 可选择的配置，不会构建或启动系统。`defconfig` 才会选中一个板级配置，`build` 负责构建，`qemu` 负责构建并运行。

## 4. 环境准备

### 4.1 使用的工具

- Rust：仓库要求的 nightly toolchain，包含 `rust-src` 和 `llvm-tools-preview`。
- AArch64 musl 交叉工具链：用于 StarryOS 及其用户态/构建辅助流程。
- QEMU：`$HOME/.local/qemu-10.2.1/bin` 中的 QEMU 10.2.1。
- Cargo：统一使用仓库的 `cargo xtask` 命令族。

### 4.2 推荐环境初始化

```bash
source "$HOME/.cargo/env"
export PATH="$HOME/.local/qemu-10.2.1/bin:$HOME/.local/toolchains/aarch64-linux-musl-cross/bin:$HOME/.local/bin:$PATH"
export PKG_CONFIG_PATH="$HOME/.local/dev-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig:$HOME/.local/dev-sysroot/usr/lib/pkgconfig:$HOME/.local/dev-sysroot/usr/share/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="$HOME/.local/dev-sysroot"
```

> ⚠️ 不要全局设置 `RUSTFLAGS` 或 `CARGO_ENCODED_RUSTFLAGS`。StarryOS 的 xtask 会通过 `--config target.<target>.rustflags=...` 注入链接脚本、PIE 和入口参数；外部 Rust flags 可能覆盖整组参数。

如果其他独立 host-side 命令确实需要额外 Rust flags，只对该条命令临时设置，不要导出到整个 shell 会话。

### 4.3 Docker 与原生环境的关系

Docker 和本次搭建的原生环境是两条平行路径：

| 路径 | 优点 | 适合场景 |
| --- | --- | --- |
| Docker | 依赖版本固定、环境可复制、接近 CI | 快速复现、多人协作、CI 验证 |
| 原生环境 | 调试直接、文件和工具访问方便、无需容器挂载 | 日常开发、GDB/日志分析、频繁迭代 |

项目中的 Docker 主要用于开发镜像、CI 和某些特殊架构验证。例如 StarryOS 文档中使用 `starryos-dev:ubuntu-qemu10.2.1`，LoongArch AxVisor 使用带 LVZ 支持的专用容器。

本次最终成功路径使用原生环境。它不是取代 Docker，而是与 Docker 对齐了关键版本和依赖；后续可以用 Docker 做复现基准，用原生环境做主要开发。

### 4.4 QEMU 版本选择

原生环境安装了 QEMU 10.2.1，并把它放在 `PATH` 前面：

```bash
export PATH="$HOME/.local/qemu-10.2.1/bin:$PATH"
qemu-system-aarch64 --version
```

这样可以与项目 StarryOS 开发容器使用的 QEMU 版本对齐。

旧版系统 QEMU 对 Day1 没有必要，但也不需要立即删除；只要通过 `PATH` 保证实际调用的是 10.2.1，它可以作为其他项目的兼容备用。排错时应同时检查：

```bash
which qemu-system-aarch64
qemu-system-aarch64 --version
```

## 5. 已完成的验证

### 5.1 AxVisor AArch64 启动

项目文档给出的第一条 AxVisor QEMU 路径是：

```bash
cargo xtask axvisor defconfig qemu-aarch64
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
cargo xtask axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

这里有三个必须对齐的对象：

1. AxVisor 自己的 build config。
2. guest 的 kernel/rootfs 镜像。
3. VM config 中的入口地址、内存布局和 guest 设备。

Day1 已确认 AxVisor AArch64 能够进入运行态。

### 5.2 AxVisor → ArceOS

ArceOS guest 在 AxVisor 下成功启动。这个结果证明了以下链路有效：

```text
QEMU AArch64
  → someboot/somehal/axplat-dyn
  → AxVisor
  → guest VM 配置
  → ArceOS guest kernel
```

### 5.3 AxVisor → Linux

Linux guest 最终验证到以下状态：

```text
VM[1] boot success
nvme nvme0
EXT4-fs (nvme0n1): mounted
VFS: Mounted root
Run /bin/sh
```

这不是“Linux 镜像被加载”这么简单，而是已经证明了：

- guest CPU 和内存启动成功；
- guest NVMe 设备被发现；
- NVMe 根盘可读；
- EXT4 根文件系统挂载成功；
- Linux 用户态 shell 成功运行。

### 5.4 StarryOS 直接启动

官方命令如下：

```bash
env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
cargo xtask starry qemu \
  --arch aarch64 \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
  --qemu-config os/StarryOS/configs/qemu/qemu-aarch64.toml \
  --smp 4
```

成功日志包含：

```text
Finished `release` profile
Converting ELF to BIN format
VM Load @0x40200000
Mapping early memory regions...
Trap vector at 0xffffffff803a7000
Welcome to Starry OS!
root@starry:/root #
```

这证明 StarryOS 已经过了以下完整阶段：

1. Rust 编译和最终链接。
2. ELF 后处理和 kallsyms 写入。
3. ELF 转换为 QEMU 使用的 BIN。
4. QEMU 加载和 AArch64 早期启动。
5. MMU、内存、trap vector 和 CPU-local 初始化。
6. StarryOS 内核入口、rootfs 和 shell 启动。

## 6. 遇到的问题与解决过程

### 6.1 AxVisor 默认配置不能直接运行

**现象：**直接从 `defconfig → build → qemu` 开始时，默认 `vm_configs` 为空，guest 镜像和 rootfs 也可能不存在。

**原因：**AxVisor 的板级默认配置只描述平台能力，不自动替用户选择具体 guest。

**解决：**使用文档提供的 `os/axvisor/scripts/setup_qemu.sh arceos` 准备 guest 镜像和 VM config，再显式传给 `cargo xtask axvisor qemu`。

### 6.2 Linux guest 的 NVMe 根盘链路

**现象：**早期 Linux 验证没有完成根文件系统进入，问题集中在 guest NVMe 驱动和根盘可见性。

**判断方法：**不能只看 Linux kernel 是否打印了启动信息，还要继续观察 `nvme nvme0`、EXT4 mount、VFS root 和 shell。

**解决：**使用修复后的、包含内建 NVMe 驱动的 guest 镜像，重新运行 AxVisor Linux 路径。

**结果：**出现 `VM[1] boot success`、`EXT4-fs ... mounted`、`VFS: Mounted root` 和 `Run /bin/sh`，Linux 验证完成。

### 6.3 StarryOS 首次构建链接失败

**失败命令：**

```bash
cargo xtask starry qemu \
  --arch aarch64 \
  --rootfs tmp/axbuild/rootfs/rootfs-aarch64-alpine.img/rootfs-aarch64-alpine.img \
  --qemu-config os/StarryOS/configs/qemu/qemu-aarch64.toml \
  --smp 4
```

**错误：**

```text
rust-lld: error: undefined symbol: _ekernel
rust-lld: error: undefined symbol: _ex_table_start
rust-lld: error: undefined symbol: _ex_table_end
rust-lld: error: undefined symbol: __PERCPU_TEMPLATE_ALIGN_START
rust-lld: error: undefined symbol: __PERCPU_TEMPLATE_ALIGN_END
rust-lld: error: undefined symbol: PAGE_SIZE
rust-lld: error: undefined symbol: STACK_SIZE
```

**第一眼的误判：**这些符号都由 `linker.x → runtime.x → axplat.x → link.x → someboot.x` 链接脚本提供，因此最初看起来像是链接脚本的 `INCLUDE` 链断了。

**进一步检查：**

- `os/StarryOS/starryos/linker.ld` 确实包含 `runtime.x`。
- `runtime.x` 确实包含 `axplat.x`。
- `axplat.x` 确实包含 `link.x`。
- `someboot.x` 中确实定义了 `PAGE_SIZE`、`STACK_SIZE` 和 `__PERCPU_TEMPLATE_ALIGN_*`。
- xtask 也确实生成了 `-Clink-args=-Tlinker.x`。

### 6.4 真正根因：外部 RUSTFLAGS 覆盖 xtask 的链接参数

当时 shell 中有：

```bash
RUSTFLAGS=-L$HOME/.local/dev-sysroot/usr/lib/x86_64-linux-gnu
```

Cargo 对 Rust flags 只选择一个来源。外部 `RUSTFLAGS` 会覆盖 xtask 通过命令行注入的 target-specific flags，导致最终 `rust-lld` 实际没有拿到完整的 `-Tlinker.x`、`-u _head` 等参数。

这正好解释了为什么多个来自链接脚本的符号同时变成 undefined，而不是某一个脚本文件找不到。

**解决：**取消两个可能覆盖配置的环境变量：

```bash
unset RUSTFLAGS
unset CARGO_ENCODED_RUSTFLAGS
```

然后重新执行官方 `cargo xtask starry qemu` 命令。

**结果：**重新编译、链接、生成 BIN，并进入 `root@starry:/root #`。

### 6.5 手动 QEMU 尝试为什么不能作为最终证据

为排除加载器问题，曾做过几次手动尝试：

| 尝试 | 结果 | 结论 |
| --- | --- | --- |
| 旧 `starryos.bin` 直接 `-kernel` | DTB 与 kernel 地址重叠 | 加载地址冲突，不能证明内核失败 |
| `dtb-random-address=on` | 当前 QEMU 不支持该属性 | 不是本项目修复方向 |
| `-device loader` 加载 BIN | 运行期间无串口输出 | 不能证明旧产物可启动 |
| 旧 ELF 直接 `-kernel` | 10 秒无串口输出 | 旧产物且未经过当前完整运行链，不计入验收 |

最终验收必须使用文档中的 `cargo xtask starry qemu`，让项目自己负责构建、kallsyms、BIN 转换、rootfs 注入和 QEMU 参数组装。

## 7. 关键证据

### 7.1 StarryOS 失败日志

带有错误环境变量时的失败日志：

- [/tmp/starry-day1-confirm.log](/tmp/starry-day1-confirm.log)

关键特征是最终链接失败，QEMU 没有真正运行。

### 7.2 StarryOS 成功日志

清理环境变量后的成功日志：

- [/tmp/starry-day1-no-rustflags.log](/tmp/starry-day1-no-rustflags.log)

关键特征是同时出现：

```text
Finished `release` profile
Welcome to Starry OS!
root@starry:/root #
```

### 7.3 成功 ELF 的关键符号

```text
_head
_ekernel
_ex_table_start
_ex_table_end
PAGE_SIZE
STACK_SIZE
__PERCPU_TEMPLATE_ALIGN_START
__PERCPU_TEMPLATE_ALIGN_END
```

这些符号证明最终 ELF 使用了正确的内核链接脚本，而不是只生成了一个无法启动的旧文件。

### 7.4 上游对比

已抓取 `rcore-os/tgoskits` 上游 `dev` 分支进行对比：

- 上游 StarryOS 的 `linker.ld` 和相关 build script 与本地实现一致。
- 上游没有一个额外的 StarryOS 链接脚本修复提交可以直接套用。
- 上游提交 `b5ec2c6ff` 已专门处理 Cargo 多个 Rust flags 来源互相覆盖的问题：额外测试 flags 会并入 target-specific 配置，避免遮蔽链接契约。

因此本次故障的责任点是本地构建环境变量，不是需要回溯上游代码的项目 bug。

## 8. Day1 验收表

| 验收项 | 状态 | 证据 |
| --- | --- | --- |
| Rust nightly 和 `rust-src` | ✅ | 可执行 `cargo xtask` 构建 |
| AArch64 交叉工具链 | ✅ | StarryOS/AxVisor AArch64 构建通过 |
| QEMU 10.2.1 | ✅ | AxVisor、Linux、StarryOS 均进入运行态 |
| AxVisor AArch64 | ✅ | hypervisor 启动成功 |
| AxVisor → ArceOS | ✅ | ArceOS guest 启动成功 |
| AxVisor → Linux | ✅ | NVMe、EXT4、VFS root、shell 全部成功 |
| QEMU → StarryOS | ✅ | `Welcome to Starry OS!` 和 shell prompt |
| Day1 启动验证的源代码修改 | 无 | 未通过修改内核或放宽测试绕过问题 |
| 新增记录 | ✅ | `docs/my/day1-progress.md` |

Day1 的结论从“StarryOS 构建受阻”更新为“StarryOS 已完成启动验收”。

## 9. 后续每日记录模板

以后每一天使用独立文件保存，例如：

```text
docs/my/day2-progress.md
docs/my/day3-progress.md
```

每个文件都沿用下面的结构：

```markdown
# TGOSKits DayN 进度记录：主题

> 日期：YYYY-MM-DD

## 1. 今日目标

## 2. 执行的命令和修改的文件

## 3. 已完成的事情

## 4. 遇到的问题

### 4.1 现象

### 4.2 根因

### 4.3 解决办法

### 4.4 验证结果

## 5. 证据

- 日志路径
- 关键输出
- 测试命令

## 6. 尚未解决的问题

## 7. 下一天的第一步
```

记录原则：每个结论都配一个命令、日志或可观察输出；失败尝试也保留，但明确标注它是否计入最终验收。
