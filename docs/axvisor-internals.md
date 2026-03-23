# AxVisor 深度技术解析

这篇文档面向准备修改 Hypervisor 运行时、VMM、vCPU 调度、板级配置、Guest 启动流程或 `axvisor_api` 的开发者。与 [axvisor-guide.md](axvisor-guide.md) 相比，这里更关注“AxVisor 为什么这样组织”和“关键路径怎样执行”。

需要先把 QEMU 路径跑通的话，请先看 [quick-start.md](quick-start.md) 和 [axvisor-guide.md](axvisor-guide.md)。

## 1. 系统定位与设计目标

AxVisor 在本仓库中的定位，是**基于 ArceOS 的统一组件化 Type-I Hypervisor**。它既不是一个“直接包裹 KVM 的用户态工具”，也不是一个单体式虚拟机管理程序，而是建立在 ArceOS 运行时、虚拟化组件库与分层配置系统之上的 Hypervisor 软件栈。

从 README 与源码可以抽象出几个核心目标：

| 目标 | 含义 | 典型落点 |
| --- | --- | --- |
| 统一 | 尽可能用同一套代码覆盖多架构平台 | `hal/arch/*`、`configs/board/*` |
| 组件化 | 将 VM、vCPU、虚拟设备、地址空间、API 注入等能力拆成独立组件 | `components/axvm`、`axvcpu`、`axdevice`、`axaddrspace`、`axvisor_api` |
| 可配置 | 通过板级配置与 VM 配置控制构建与运行行为 | `configs/board/*.toml`、`configs/vms/*.toml`、`.build.toml` |
| 可验证 | 通过本地 xtask、`setup_qemu.sh`、QEMU workflow 和统一测试入口形成闭环 | `os/axvisor/xtask`、`.github/workflows/*.toml`、根 `cargo xtask test axvisor` |

从开发体验上看，AxVisor 与 ArceOS/StarryOS 的最大差异在于：**代码、配置和 Guest 镜像同等重要**。很多“看起来像代码 bug”的问题，根因其实是 `.build.toml`、`vm_configs`、`kernel_path` 或 `tmp/rootfs.img` 没有对齐。

## 2. 总体架构与分层设计

AxVisor 的运行结构可以理解为“ArceOS 作为宿主运行时 + 虚拟化组件作为能力核 + AxVisor 运行时负责编排 + Guest 作为最终负载”。

```mermaid
flowchart LR
    hardware["Hardware: QEMU 或真实开发板"]
    arceosBase["ArceosBase: axstd axhal axalloc axtask axsync"]
    virtComponents["VirtComponents: axvm axvcpu axdevice axaddrspace"]
    apiBridge["ApiBridge: axvisor_api + hal impl"]
    axvisorRuntime["AxvisorRuntime: main hal vmm task shell driver"]
    guestSystems["GuestSystems: ArceOS Linux NimbOS"]
    boardConfig["BoardConfig: configs/board/*.toml"]
    vmConfig["VmConfig: configs/vms/*.toml 或 /guest/vm_default/*.toml"]

    hardware --> arceosBase
    arceosBase --> apiBridge
    arceosBase --> axvisorRuntime
    virtComponents --> axvisorRuntime
    apiBridge --> virtComponents
    axvisorRuntime --> guestSystems
    boardConfig --> axvisorRuntime
    vmConfig --> axvisorRuntime
    vmConfig --> virtComponents
```

这张图可以从两条主线来读：

- **运行主线**：`hardware -> ArceOS base -> virt components -> Axvisor runtime -> guests`
- **配置主线**：`board config + vm config -> Axvisor runtime / virt components`

也就是说，AxVisor 的实际行为既取决于 Rust 代码，也取决于运行时加载进来的 TOML 配置。

### 2.1 主要分层职责

| 层次 | 目录 | 职责 |
| --- | --- | --- |
| 宿主运行时层 | `axstd`、`axhal`、`axalloc`、`axtask` | 提供宿主机上的调度、内存、时间、控制台与硬件抽象 |
| 虚拟化能力层 | `components/axvm`、`axvcpu`、`axdevice`、`axaddrspace` | 抽象 VM、vCPU、设备模拟/直通与客户机地址空间 |
| API 注入层 | `components/axvisor_api`、`src/hal` 中的 `api_mod_impl` | 将 ArceOS 的能力注入到更底层虚拟化组件 |
| AxVisor 编排层 | `os/axvisor/src/*` | 初始化、VMM、shell、任务组织、Guest 启停 |
| 配置与镜像层 | `configs/board/*`、`configs/vms/*`、`tmp/*`、镜像仓库 | 控制“构建什么”和“启动哪个 Guest” |

### 2.2 板级与平台现状

当前仓库中的板级配置文件包括：

- `qemu-aarch64.toml`
- `qemu-x86_64.toml`
- `orangepi-5-plus.toml`
- `phytiumpi.toml`
- `roc-rk3568-pc.toml`

对于本地开发，最稳妥的路径仍然是 `qemu-aarch64`：

- 当前仓库文档与 CI 主要围绕这条路径组织。
- `setup_qemu.sh` 对 `arceos` 和 `linux` 两类 AArch64 Guest 支持最直接。
- 在 WSL2 或无硬件虚拟化环境下，也更适合先使用纯软件仿真路径。

## 3. 核心设计理念与实现机制

### 3.1 运行时主线非常短，但 VMM 很深

`os/axvisor/src/main.rs` 中的 `main()` 很短：

```rust
fn main() {
    logo::print_logo();
    info!("Starting virtualization...");
    info!("Hardware support: {:?}", axvm::has_hardware_support());
    hal::enable_virtualization();
    vmm::init();
    vmm::start();
    shell::console_init();
}
```

这段代码说明 AxVisor 的运行时主线可以概括为四步：

1. 使能硬件虚拟化支持。
2. 初始化 VMM 和 Guest VM 描述。
3. 启动 VM。
4. 进入交互 shell。

真正的复杂度集中在 `hal`、`vmm` 和配置解析，而不是主函数自身。

### 3.2 配置优先于代码执行

`vmm::init()` 会先调用 `config::init_guest_vms()`。这个过程会优先从文件系统读取 `/guest/vm_default/*.toml`，如果没有，再回退到静态内置配置。之后会对每一份配置执行：

- 解析 TOML 为 `AxVMCrateConfig`
- 构造 `AxVMConfig`
- 创建 VM 实例
- 分配或映射客户机物理内存
- 配置 kernel load address
- 加载 kernel / dtb / ramdisk / disk 等镜像
- 调用 `vm.init()`
- 将 VM 状态置为 `Loaded`

这意味着 Guest 的存在方式不是“代码里写死一个默认 VM”，而是“配置驱动的 VM 实例化过程”。

### 3.3 vCPU 不只是对象，更是 ArceOS task

AxVisor 的另一个关键设计是：每个 vCPU 最终都会被包装成 ArceOS task，并进入自己的等待队列与运行循环中。`vcpus.rs` 里的重要事实包括：

- 主 vCPU 会在 `setup_vm_primary_vcpu()` 中首先被分配 task。
- vCPU task 初始是阻塞的，直到 `notify_primary_vcpu()` 唤醒。
- `vcpu_run()` 中会不断调用 `vm.run_vcpu()` 并处理不同的 `AxVCpuExitReason`。
- VM 停止时，最后一个退出的 vCPU 会把 VM 状态推进到 `Stopped`。

这意味着 AxVisor 的并发模型可以理解为：

- 宿主侧由 ArceOS task 负责调度。
- 客户机侧由 VMM 抽象出的 vCPU 状态机负责执行。
- 二者通过 `vcpu_run()` 这一桥接循环耦合在一起。

### 3.4 `axvisor_api`：把宿主能力注入到底层组件

`axvisor_api` 的设计目标是替代大量泛型 trait 传递，把底层组件需要的宿主能力按模块分类暴露为统一 API。

它解决的核心问题是：

- 底层虚拟化组件需要访问宿主内存、时间、VMM 信息等能力。
- 这些组件不应该直接依赖整个 ArceOS。
- 如果继续靠 trait 泛型层层传递，类型签名会越来越重，维护成本高。

因此 `axvisor_api` 采用了 `crate_interface + api_mod` 的设计，把 API 按功能域分组，例如：

- `memory`
- `time`
- `vmm`
- `host`

而 AxVisor 本体在 `src/hal/mod.rs` 中通过 `#[axvisor_api::api_mod_impl(...)]` 提供这些 API 的真实实现，把 ArceOS 的分配器、时间源、CPU 信息、VM/vCPU 信息等注入给底层组件。

## 4. 主要功能组件与模块划分

### 4.1 AxVisor 运行时模块

| 模块 | 目录 | 职责 |
| --- | --- | --- |
| 入口与编排 | `src/main.rs` | 按顺序触发硬件虚拟化、VMM 初始化、VM 启动与 shell |
| `hal` | `src/hal/*` | 适配具体架构，提供 `AxVMHalImpl`、`AxVCpuHalImpl`、`AxMmHalImpl` 和 `axvisor_api` 实现 |
| `vmm` | `src/vmm/*` | 配置解析、VM 列表、镜像加载、vCPU 管理、timer、hypercall |
| `task` | `src/task/*` | vCPU task 的扩展信息与调度辅助 |
| `shell` | `src/shell/*` | 控制台命令解释与 VM 管理交互 |
| `driver` | `src/driver/*` | Hypervisor 宿主侧设备接入与支持逻辑 |

### 4.2 配置与工具模块

| 模块 | 目录 | 作用 |
| --- | --- | --- |
| 板级配置 | `configs/board/*.toml` | 定义 target、features、日志级别、默认 `vm_configs` 等 |
| VM 配置 | `configs/vms/*.toml` | 定义 VM 基本信息、内核镜像、内存、设备、直通与排除项 |
| 本地构建工具 | `xtask/src/main.rs` | 提供 `defconfig`、`build`、`qemu`、`menuconfig`、`vmconfig`、`image` 等命令 |
| QEMU 快速准备脚本 | `scripts/setup_qemu.sh` | 下载镜像、生成 VM config、复制 rootfs |

### 4.3 关键配置字段

`qemu-aarch64.toml` 这类板级配置通常包含：

- `features`
- `log`
- `target`
- `to_bin`
- `vm_configs`

而单个 VM 配置则通常分为三段：

| 配置段 | 说明 |
| --- | --- |
| `[base]` | VM id、name、vm_type、CPU 数和物理 CPU 绑定 |
| `[kernel]` | entry point、image location、kernel path、load address、memory regions |
| `[devices]` | passthrough devices、excluded devices、emu devices、interrupt mode |

## 5. 关键执行场景分析

### 5.1 从配置到 Guest 启动的构建与运行链路

下面的流程图适合回答“为什么只执行 `cargo axvisor build/qemu` 往往还不够”：

```mermaid
flowchart TD
    selectBoard["SelectBoard: cargo xtask defconfig qemu-aarch64"]
    buildToml["BuildToml: 生成 .build.toml"]
    buildKernel["BuildKernel: cargo xtask build"]
    setupGuest["SetupGuest: ./scripts/setup_qemu.sh arceos"]
    downloadImage["DownloadImage: 下载并解压 guest 镜像"]
    genVmconfig["GenVmconfig: 生成 tmp/vmconfigs/*.generated.toml"]
    copyRootfs["CopyRootfs: 复制 rootfs.img 到 tmp/rootfs.img"]
    runQemu["RunQemu: cargo xtask qemu --build-config --qemu-config --vmconfigs"]
    bootGuest["BootGuest: Guest 输出启动信息"]

    selectBoard --> buildToml
    buildToml --> buildKernel
    buildKernel --> setupGuest
    setupGuest --> downloadImage
    downloadImage --> genVmconfig
    genVmconfig --> copyRootfs
    copyRootfs --> runQemu
    runQemu --> bootGuest
```

这条链路里有两个高频坑点：

- `.build.toml` 只决定“Hypervisor 怎么构建”，并不会自动准备 Guest 镜像。
- `qemu-aarch64.toml` 默认 `vm_configs = []`，因此如果你没有额外传入生成后的 VM config，`qemu` 根本不知道要启动哪个 Guest。

### 5.2 VMM 初始化与 VM 启动时序

下面这张时序图描述了 VM 从“配置文本”变成“开始执行 vCPU”的关键过程：

```mermaid
sequenceDiagram
    participant Main
    participant Hal
    participant Vmm
    participant Config
    participant Vm
    participant VcpuTask

    Main->>Hal: enable_virtualization()
    Main->>Vmm: init()
    Vmm->>Config: init_guest_vms()
    Config->>Config: 读取文件系统或静态 VM 配置
    Config->>Vm: VM::new + 分配内存 + 加载镜像 + vm.init()
    Config-->>Vmm: VM 状态进入 Loaded
    Vmm->>VcpuTask: setup_vm_primary_vcpu()
    Main->>Vmm: start()
    Vmm->>Vm: vm.boot()
    Vmm->>VcpuTask: notify_primary_vcpu()
    VcpuTask->>Vm: run_vcpu() 循环执行
```

从调试角度看，这张图很关键，因为它帮助你区分：

- 问题出在“配置解析”阶段。
- 问题出在“镜像装载/内存分配”阶段。
- 问题出在“vCPU 线程已创建但未被唤醒”阶段。

### 5.3 vCPU 运行循环与 VM Exit 处理

`vcpu_run()` 是 AxVisor 动态行为最密集的地方。它会不断处理 `Hypercall`、`ExternalInterrupt`、`Halt`、`CpuUp`、`SystemDown`、`SendIPI` 等退出原因。

```mermaid
flowchart TD
    waitRun["WaitRun: 等待 VM 进入 Running"]
    runLoop["RunLoop: vm.run_vcpu(vcpu_id)"]
    hypercall["Hypercall: 构造 HyperCall 并执行"]
    irqExit["ExternalInterrupt: 转交宿主中断处理"]
    haltExit["Halt/CpuDown: 进入 wait queue"]
    cpuUpExit["CpuUp: 创建目标 vCPU task"]
    downExit["SystemDown or Error: vm.shutdown()"]
    suspendCheck["SuspendCheck: suspending 时等待 resume"]
    stopCheck["StopCheck: stopping 时退出并更新状态"]

    waitRun --> runLoop
    runLoop --> hypercall
    runLoop --> irqExit
    runLoop --> haltExit
    runLoop --> cpuUpExit
    runLoop --> downExit
    hypercall --> suspendCheck
    irqExit --> suspendCheck
    haltExit --> suspendCheck
    cpuUpExit --> suspendCheck
    downExit --> stopCheck
    suspendCheck --> stopCheck
    stopCheck --> runLoop
```

这张图适合用来分析：

- 某个 Guest 为什么看起来“启动了但没反应”。
- 是不是卡在 `Halt` 或 `CpuDown` 等待唤醒。
- 是不是频繁陷入 `Hypercall` 或 `ExternalInterrupt` 路径。

### 5.4 VM 生命周期

AxVisor shell 与 VMM 代码中都围绕 `VMStatus` 做状态判断。常见状态包括 `Loading`、`Loaded`、`Running`、`Suspended`、`Stopping`、`Stopped`。

```mermaid
stateDiagram-v2
    [*] --> Loading
    Loading --> Loaded: 配置解析 内存建立 镜像装载完成
    Loaded --> Running: vm start 或启动时自动 boot
    Running --> Suspended: vm suspend
    Suspended --> Running: vm resume
    Running --> Stopping: shutdown error stop
    Loaded --> Stopping: stop before run
    Stopping --> Stopped: 最后一个 vCPU 退出
    Stopped --> Running: vm start
    Stopped --> [*]
```

这张图有助于理解 shell 命令为什么会限制某些状态转换，例如：

- `Loaded` 不能直接 `resume`，只能 `start`。
- `Suspended` 不能重复 `suspend`。
- `Stopping` 期间通常需要等待 vCPU 真正退出。

## 6. 开发环境与构建指南

### 6.1 最小环境

AxVisor README 推荐的环境准备包括：

- Linux 开发环境。
- `libssl-dev`、`gcc`、`libudev-dev`、`pkg-config` 等基础包。
- Rust 工具链。
- `cargo-binutils`。
- 需要构建某些 Guest 应用时，额外准备 Musl 工具链。

如果你的目标只是先跑通 QEMU AArch64 路径，最关键的是：

- QEMU 可用。
- 能运行 `cargo xtask image download ...` 或 `setup_qemu.sh` 下载镜像。
- 不要假设 `defconfig/build` 会自动生成 rootfs。

### 6.2 本地 xtask 是主入口

与 ArceOS / StarryOS 不同，AxVisor 的 build/qemu 不走根 `tg-xtask`，而是 `os/axvisor` 自带 xtask。

两种等价入口如下：

```bash
# 根目录
cargo axvisor defconfig qemu-aarch64
cargo axvisor build

# 子目录
cd os/axvisor
cargo xtask defconfig qemu-aarch64
cargo xtask build
```

常用子命令包括：

- `defconfig`
- `build`
- `qemu`
- `menuconfig`
- `vmconfig`
- `image`

### 6.3 推荐的第一条成功路径

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果你只执行：

```bash
cargo axvisor defconfig qemu-aarch64
cargo axvisor build
cargo axvisor qemu
```

则很可能还缺：

- `.build.toml` 之外的实际 VM config
- `tmp/rootfs.img`
- 已修正 `kernel_path` 的 generated vmconfig

## 7. 核心 API、配置与命令使用说明

### 7.1 板级配置与 VM 配置

`qemu-aarch64.toml` 一类板级配置主要控制：

- 构建目标 triple
- feature 组合
- 日志级别
- 是否生成 bin
- 默认 `vm_configs`

而单个 VM 配置则通常长这样：

```toml
[base]
id = 1
name = "arceos-qemu"
cpu_num = 1

[kernel]
entry_point = 0x8020_0000
image_location = "memory"
kernel_path = "path/arceos-aarch64-dyn-smp1.bin"
kernel_load_addr = 0x8020_0000
memory_regions = [
  [0x8000_0000, 0x4000_0000, 0x7, 1],
]

[devices]
passthrough_devices = [["/",]]
excluded_devices = [["/pcie@10000000"]]
interrupt_mode = "passthrough"
```

对开发者最重要的是搞清楚：

- `board` 配置控制 Hypervisor 自己。
- `vm` 配置控制 Guest。
- 两者缺一不可。

### 7.2 `axvisor_api` 的使用方式

`axvisor_api` 通过 `api_mod` 暴露统一接口模块，调用方看到的是普通函数风格，而不是一长串 trait 泛型。

例如一个内存 API 模块的设计形态是：

```rust
#[api_mod]
mod memory {
    extern fn alloc_frame() -> Option<PhysAddr>;
    extern fn dealloc_frame(addr: PhysAddr);
}
```

而 AxVisor 本体在 `src/hal/mod.rs` 中提供实现，把 `axalloc`、`axhal` 等真实宿主能力接进去。

这种方式的好处在于：

- 调用者不必把所有宿主能力写进类型参数。
- API 可以按功能域组织。
- 底层组件不需要直接依赖整个 ArceOS。

### 7.3 Shell 与运行时管理

AxVisor 启动完默认 Guest 后，会进入控制台 shell：

- 支持命令历史。
- 能对 VM 执行查询和管理命令。
- 在启用了文件系统支持时，提示符还会显示当前目录。

因此 AxVisor 不是“一启动就结束”的批处理式 Hypervisor，而是一个带有交互式宿主管理面的系统软件。

## 8. 调试、故障排查与优化方法

### 8.1 先看配置，再看镜像，再看代码

AxVisor 排障最有效的顺序通常是：

1. `.build.toml` 是否对应正确板级配置。
2. `vm_configs` 是否为空。
3. `kernel_path`、`bios_path`、`rootfs.img` 是否真实存在。
4. 镜像入口地址、加载地址、内存区域是否匹配。
5. 最后才回到 `vmm`、`hal` 或 `vcpus` 代码本身。

### 8.2 常见问题

| 现象 | 常见原因 | 建议排查点 |
| --- | --- | --- |
| `cargo axvisor qemu` 直接失败 | 没有准备 generated vmconfig 与 `tmp/rootfs.img` | 优先使用 `setup_qemu.sh` |
| QEMU 启动了但没有 Guest 输出 | `vm_configs` 为空或 `kernel_path` 错误 | 看 `configs/board/*` 与 `tmp/vmconfigs/*` |
| VM 创建失败 | TOML 不合法、内存区域不合理、镜像缺失 | 看 `vmm/config.rs` 中的 `init_guest_vm()` |
| vCPU 任务没有运行 | 只创建了 task 但未唤醒，或 VM 状态没进入 Running | 看 `notify_primary_vcpu()` 与 `vm.boot()` |
| WSL2 / 无硬件虚拟化环境下 x86 路径异常 | KVM/VT-x 不可用 | 优先走 AArch64 QEMU 纯软件路径 |

### 8.3 调试命令

```bash
# 重新生成板级配置
cargo axvisor defconfig qemu-aarch64

# 交互式修改配置
cargo axvisor menuconfig

# 只做构建，先排除编译问题
cargo axvisor build

# 下载/准备镜像与 rootfs
cd os/axvisor
./scripts/setup_qemu.sh arceos

# 统一测试入口
cd /home/chyyuu/thecodes/tgoskits
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

### 8.4 优化切入点

如果你准备优化 AxVisor，常见方向包括：

- vCPU exit 热路径：分析 `vcpu_run()` 中的高频 exit reason。
- 配置与镜像加载：减少重复解析和不必要的 I/O。
- 中断与 timer：分析 `ExternalInterrupt` 和 timer 回调对延迟的影响。
- API 注入层：评估 `axvisor_api` 的可维护性与可扩展性，而不是只看运行时性能。

## 9. 二次开发建议与阅读路径

建议按下面顺序继续深入：

1. 从 `src/main.rs`、`src/hal/mod.rs`、`src/vmm/mod.rs` 看主运行链。
2. 再看 `src/vmm/config.rs` 和 `src/vmm/vcpus.rs`，理解配置如何变成 VM 与 vCPU。
3. 如果你要扩展底层组件，继续看 `components/axvm`、`axvcpu`、`axdevice`、`axaddrspace`。
4. 如果你要改 API 注入方式，继续看 `components/axvisor_api/README.zh-cn.md` 与 `src/hal/mod.rs` 中的 `api_mod_impl`。

关联阅读建议：

- [axvisor-guide.md](axvisor-guide.md)：更偏“上手命令、配置路径和 Guest 准备”。
- [build-system.md](build-system.md)：更偏“根工作区入口与 AxVisor 本地 xtask 的边界”。
- [arceos-internals.md](arceos-internals.md)：更偏“AxVisor 复用的宿主运行时和底层模块能力”。
