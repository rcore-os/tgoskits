# Axvisor ArceOS API 依赖重构方案

## 背景与目标

当前 ArceOS 对外同时存在多类 API 入口：

- `ax-api`：ArceOS 原生公共 API，覆盖 console、fs、task、time、display 等能力。
- `ax-std`：类 Rust `std` 的用户态/应用侧 facade。
- `ax-api::modules` 以及底层 crate：例如 `ax_hal`、`ax_task`、`ax_alloc`、`ax_fs`。
- `arceos-rust`：面向 Rust `std` 兼容的过渡实现，当前更接近 Hermit target runtime shim，不是最终 `target_os = "arceos"` 的 native std 实现。

普通 ArceOS 应用通常只需要 `ax_std::{fs, io, thread, sync, time, net, ...}`。但 Axvisor 不是普通应用，它需要：

- CPU 数量、当前 CPU、CPU affinity；
- IRQ 分发和虚拟中断注入；
- 页表、地址转换、frame/连续页/DMA 分配；
- per-cpu 虚拟化状态；
- vCPU task 绑定；
- timer、tick、wall time；
- 平台生命周期和硬件虚拟化开关；
- 架构相关的 EL2/VMX/SVM/H 扩展能力。

因此 Axvisor 无法只通过 `ax_std::{thread, fs, io}` 实现，也不适合让业务层直接散落调用：

```rust
ax_api
ax_hal
ax_task
ax_alloc
ax_std::os::arceos::modules::ax_hal
ax_std::os::arceos::modules::ax_task
```

从当前代码看，Axvisor 对 ArceOS 能力的访问并未统一，而是同时存在三种路径：

```text
1. 通过 ax_std::os::arceos::api 使用 ax-api

   extern crate ax_std as std;
   use std::os::arceos::api::time::ax_wall_time;
   use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};

2. 通过 ax_std::os::arceos::modules 间接使用 ax-api::modules

   extern crate ax_std as std;
   use std::os::arceos::modules::ax_task::{TaskExt, TaskInner};
   use std::os::arceos::modules::ax_fs;
   use std::os::arceos::modules::ax_hal;

3. 直接依赖底层 ArceOS crate

   use ax_hal;
   use ax_task::{AxTaskRef, TaskInner, WaitQueue};
   use ax_hal::{dtb, mem};
   use ax_hal::time::busy_wait;
```

其中第 2 类路径本质上是：

```text
ax_std::os::arceos::modules
    -> ax-api::modules
        -> ax_hal / ax_task / ax_fs / ...
```

也就是说，当前 Axvisor 并不是直接统一通过 `ax-api::modules` 使用 ArceOS 组件；它只是有一部分经由 `ax-std` 间接使用了 `ax-api::modules`，同时仍保留不少对 `ax_hal`、`ax_task` 等底层 crate 的直接引用。重构目标是将这些路径收敛到 `crate::host` 内部，由 `host` 再统一选择使用 `ax_std::os::arceos::{api, modules}`。

同时，Axvisor 还包含大量 `virtualization/*` 组件。这里的“ArceOS 组件 API”主要指：

```text
ax_api
ax-api::modules
ax_std::os::arceos::{api, modules}
ax_hal / ax_task / ax_alloc / ax_fs 等底层 ArceOS 模块入口
```

这些 virtualization 组件与 ArceOS 组件 API 的关系并不相同：

```text
virtualization/axvm
  VM runtime/domain 组件。
  不应直接使用 ArceOS 组件 API；当前通过 axvisor_api 间接请求 host memory/vmm 等能力。

virtualization/axvmconfig
  VM 配置 schema/parser/tool。
  不直接使用 ArceOS 组件 API，也不通过 axvisor_api 请求 host 能力；它主要提供配置结构和解析能力。

virtualization/axvcpu
  vCPU 抽象。
  不直接使用 ArceOS 组件 API；当前依赖 axvisor_api 的 VMId/VCpuId 等类型。

virtualization/*_vcpu
  架构 vCPU 实现，例如 arm_vcpu、x86_vcpu、riscv_vcpu、loongarch_vcpu。
  不应直接使用 ArceOS 组件 API；当前通过 axvisor_api 间接请求 memory、arch、time、vmm 等 host 能力。

memory/axaddrspace
  guest address space 组件。
  不直接使用 ArceOS 组件 API，也不通过 axvisor_api 请求 host 能力；它依赖地址、页表、内存集合等基础组件。

virtualization/axdevice*
  虚拟设备框架。
  axdevice_base 不直接使用 ArceOS 组件 API；
  axdevice 本身主要组合设备模型，部分具体中断控制器/定时器设备组件会通过 axvisor_api 间接请求 host memory/time/vmm 能力。

virtualization/axvisor_api
  virtualization 组件访问 host 能力的 capability API。
  它自身不直接使用 ArceOS 组件 API；真正的 ArceOS 适配由 os/axvisor 中的 api_impl 完成。
```

这些 virtualization 组件不能直接依赖 `os/axvisor/src/host`，也不应该直接依赖 ArceOS，否则它们会退化为 Axvisor-on-ArceOS 的内部实现，无法保持组件独立性。

基于上述现状，本方案聚焦当前 Axvisor 对 ArceOS API 的依赖重构，先解决当前最直接的问题：

1. 统一 Axvisor 对 ArceOS 能力的使用入口。
2. 明确 `ax-std`、`ax-api`、`axvisor_api`、`crate::host` 的边界。
3. 降低 Axvisor 业务层对 ArceOS 底层 crate 的直接耦合。

目标是将 Axvisor 对 ArceOS 的依赖收敛为清晰的分层结构：

```text
普通 ArceOS 应用
    -> ax_std::{fs, io, thread, sync, time, net, ...}

复杂系统 / privileged subsystem
    -> 自己的边界层
        -> ax_std::os::arceos::{api, modules}

os/axvisor 业务层
    -> crate::host
        -> ax_std::os::arceos::{api, modules}

virtualization 组件
    -> axvisor_api
        -> os/axvisor::host::api_impl
            -> crate::host
                -> ax_std::os::arceos::{api, modules}
```

重构完成后，希望达到以下效果：

- `ax-std` 成为 ArceOS 对外统一 SDK facade。
- Axvisor 业务层只通过 `crate::host` 使用 host 能力。
- `virtualization/*` 组件通过 `axvisor_api` 访问 host capability，不直接绑定 ArceOS。
- `os/axvisor/src/hal` 不再作为业务层入口，能力逐步迁入 `os/axvisor/src/host`。
- `main.rs`、`vmm/*`、`shell/*` 中不再直接散落 `ax_api`、`ax_hal`、`ax_task`、`ax_alloc`、`ax_std::os::arceos`。
- Axvisor 对 ArceOS 的直接依赖收敛到 `crate::host`，业务层引用路径更短，架构边界更清楚。
- `ax_std::os::arceos` 的形态贴近未来 `std::os::arceos`，为后续 ArceOS native `std` 支持预留路径。

## 方案

### 1. `ax-std` 作为 ArceOS 统一 SDK facade

`ax-std` 的定位从“少量类 std 工具库”提升为 ArceOS 统一 SDK facade：

```text
ax_std::{fs, io, thread, sync, time, net, process, env, ...}
    面向普通应用的 std-like API

ax_std::os::arceos::api
    ArceOS 原生公共 API，复杂系统优先使用

ax_std::os::arceos::modules
    ArceOS 底层模块 escape hatch，供 Axvisor 等复杂系统使用
```

公开结构建议保持接近 Rust `std`：

```text
ax_std/
├── env
├── fs
├── io
├── net
├── os
│   └── arceos
│       ├── api         # re-export ax-api
│       ├── modules     # re-export ax-api::modules
│       ├── config      # optional: re-export api::config
│       └── prelude     # optional: common ArceOS-specific imports
├── process
├── sync
├── thread
└── time
```

核心导出形态：

```rust
pub mod os {
    pub mod arceos {
        pub use ax_api as api;
        pub use ax_api::modules;

        pub mod prelude {
            pub use super::api::{AxError, AxResult};
        }
    }
}
```

稳定性分层：

```text
Tier A: ax_std::{fs, io, thread, sync, time, net, ...}
  普通应用 API，尽量保持 std-like 稳定。

Tier B: ax_std::os::arceos::api
  ArceOS 原生公共 API，复杂系统优先使用。

Tier C: ax_std::os::arceos::modules
  底层模块 escape hatch，允许 Axvisor 使用，但稳定性弱于 Tier A/B。
```

使用规则：

- 普通应用优先使用 Tier A。
- 复杂系统优先使用 Tier B。
- 只有 Tier B 不足以表达底层能力时才使用 Tier C。
- Tier C 不应在业务层散落使用，应由 subsystem 自己的 facade 统一封装。

`ax-std` feature 继续承担 ArceOS 能力聚合：

```toml
ax-std = { workspace = true, features = [
    "paging",
    "irq",
    "multitask",
    "task-ext",
    "smp",
    "hv",
    "buddy-slab",
    "fs",
] }
```

feature 分层建议：

```text
std-like features:
  alloc, fs, net, dns, display, multitask, sync, thread, time

system features:
  paging, dma, tls, irq, smp, hv, task-ext

allocator/scheduler features:
  alloc-tlsf, alloc-slab, alloc-buddy, buddy-slab
  sched-fifo, sched-rr, sched-cfs

platform/driver passthrough:
  不建议无限纳入 ax-std；由构建配置或具体系统控制
```

### 2. Axvisor 通过 `crate::host` 使用 ArceOS 能力

`crate::host` 是 Axvisor 对 ArceOS 的唯一直接使用边界。业务层不直接依赖 `ax_std::os::arceos::{api, modules}`。

目标调用关系：

```text
main / vmm / shell
    -> crate::host::{console, fs, cpu, irq, memory, task, time, timer, platform, ...}
        -> ax_std::os::arceos::{api, modules}
```

`host/mod.rs` 公开 facade：

```rust
pub mod cache;
pub mod console;
pub mod cpu;
pub mod fs;
pub mod irq;
pub mod memory;
pub mod paging;
pub mod percpu;
pub mod platform;
pub mod task;
pub mod time;
pub mod timer;

mod arch;
mod api_impl;

pub use ax_errno::{AxError, AxResult as HostResult};
```

`arch` 与 `api_impl` 是私有实现细节，不作为 `crate::host` 公共 API 暴露。

### 3. `host` 模块职责

```text
host/console.rs
  console 输出封装，内部使用 ax_std::os::arceos::api::stdio。

host/fs.rs
  配置、镜像、目录读取封装，内部可使用 ax_std::fs 或 ax_std::os::arceos::api::fs。

host/cpu.rs
  CPU 数量、当前 CPU、CPU affinity。

host/task.rs
  vCPU task spawn/current/join、TaskExt、WaitQueue 的唯一封装点。

host/memory.rs
  frame、连续页、DMA、phys/virt 地址转换。

host/paging.rs
  PagingHandler、AxMmHal 等页表适配。

host/irq.rs
  host IRQ 分发、虚拟中断注入入口。

host/time.rs
  tick/nanos/wall time 等底层时间能力。

host/timer.rs
  Axvisor host timer service。

host/platform.rs
  平台生命周期、硬件虚拟化检查、enable virtualization、system off。

host/cache.rs
  cache maintenance 统一入口。

host/percpu.rs
  Axvisor per-cpu virtualization state。

host/arch/*
  架构相关 hook，只供 host 内部调用。

host/api_impl/*
  私有：实现 axvisor_api traits，不向业务层暴露。
```

### 4. `axvisor_api` 作为 virtualization capability API

`axvisor_api` 不应被理解为 ArceOS API，也不是 `ax-std` 的替代品。它的职责是让 `virtualization/*` 组件访问 host capability：

```text
virtualization/axvm
virtualization/axvcpu
virtualization/arm_vgic
virtualization/x86_vlapic
virtualization/*_vcpu
    -> axvisor_api
        -> os/axvisor::host::api_impl
            -> crate::host
                -> ax_std::os::arceos::{api, modules}
```

保留 `axvisor_api` 的原因：

- `axvm`、`axvcpu`、`vgic`、`vlapic` 等组件不能依赖 `os/axvisor/src/host`。
- 这些组件也不应直接依赖 ArceOS。
- 若删除 `axvisor_api`，只能改成直接依赖 ArceOS、直接依赖 Axvisor 内部模块，或向多层组件传递大量泛型 trait，都会增加耦合或复杂度。

`host/api_impl` 只负责注册 trait 实现：

```text
hal/impl_host.rs    -> host/api_impl/host.rs
hal/impl_memory.rs  -> host/api_impl/memory.rs
hal/impl_time.rs    -> host/api_impl/time.rs
hal/impl_vmm.rs     -> host/api_impl/vmm.rs
```

这些实现内部只调用 `crate::host::*`，不形成业务层可调用 API。

### 5. virtualization 组件分层

建议明确 `virtualization/*` 的分层：

```text
基础 virtualization 组件：
  axaddrspace
  axvcpu
  arch-specific vcpu crates
  axdevice_base
  axvmconfig

虚拟化运行时组件：
  axvm
  axdevice
  arm_vgic
  x86_vlapic
  riscv_vplic

Host capability API：
  axvisor_api

ArceOS 适配层：
  os/axvisor/src/host
  os/axvisor/src/host/api_impl

Axvisor 业务层：
  os/axvisor/src/main.rs
  os/axvisor/src/vmm
  os/axvisor/src/shell
```

`virtualization/axvm` 有必要保留。它应承担 VM runtime/domain 责任：

- VM 生命周期；
- vCPU 管理；
- VM memory region；
- address space 绑定；
- device model 接入；
- 调用 `axvcpu` 和架构 vCPU；
- 通过 `axvisor_api` 请求 host memory/time/irq/vmm 能力。

`axvm` 不应直接调用 `ax_std::os::arceos::{api, modules}`、`ax_api`、`ax_hal`、`ax_task` 等 ArceOS 组件 API；这些能力应由 `os/axvisor/src/host/api_impl` 实现 `axvisor_api` 后间接提供。

`virtualization/axvmconfig` 作为 VM 配置组件保留：

- no_std schema；
- serde 配置结构；
- 配置校验；
- TOML 解析；
- CLI、模板生成、schema dump。

`axvmconfig` 不负责访问 host memory、task、irq、time 等能力，因此不应直接或间接依赖 ArceOS 组件 API。

### 6. 最终目录结构

```text
os/axvisor/src/
├── main.rs                         # Axvisor 启动入口，只编排 host/vmm/shell
├── task.rs                         # vCPU task 扩展数据类型，避免业务状态塞进 host
├── host/                           # Axvisor-on-ArceOS host boundary
│   ├── mod.rs                      # host facade 公共出口，私有挂载 arch/api_impl
│   ├── console.rs                  # console 输出封装
│   ├── fs.rs                       # 配置/镜像文件访问封装
│   ├── cpu.rs                      # CPU 数量、当前 CPU、CPU affinity
│   ├── irq.rs                      # host IRQ 分发和中断注入入口
│   ├── memory.rs                   # frame、连续页、DMA、地址转换
│   ├── task.rs                     # TaskInner/TaskExt/spawn/current/join 封装点
│   ├── time.rs                     # tick/nanos/wall time
│   ├── timer.rs                    # Axvisor host timer service
│   ├── platform.rs                 # 平台生命周期、虚拟化开启、system off
│   ├── paging.rs                   # PagingHandler/AxMmHal 页表适配
│   ├── cache.rs                    # cache maintenance 统一入口
│   ├── percpu.rs                   # AxVMPerCpu 和硬件虚拟化 per-cpu 状态
│   ├── api_impl/                   # 私有：注册 axvisor_api trait 实现
│   │   ├── mod.rs                  # 汇总 private impl modules
│   │   ├── host.rs                 # HostIf 实现
│   │   ├── memory.rs               # MemoryIf 实现
│   │   ├── time.rs                 # TimeIf 实现
│   │   └── vmm.rs                  # VmmIf 实现
│   └── arch/                       # 私有：架构相关 host hook
│       ├── mod.rs                  # 按 target_arch 选择架构实现
│       ├── aarch64/
│       │   ├── mod.rs              # AArch64 虚拟化/IRQ/平台 hook
│       │   ├── cache.rs            # AArch64 cache maintenance
│       │   └── api.rs              # AArch64 axvisor_api::arch 实现
│       ├── riscv64/
│       │   ├── mod.rs              # RISC-V 虚拟化/IRQ/平台 hook
│       │   └── cache.rs            # RISC-V cache maintenance
│       ├── x86_64/
│       │   ├── mod.rs              # x86_64 VMX/SVM/IRQ/平台 hook
│       │   └── cache.rs            # x86_64 cache maintenance
│       └── loongarch64/
│           ├── mod.rs              # LoongArch64 LVZ/IRQ/平台 hook
│           └── cache.rs            # LoongArch64 cache maintenance
├── vmm/                            # VM/vCPU 业务逻辑，只依赖 host facade 和 virtualization 组件
│   ├── mod.rs                      # VMM 初始化、启动、VM/vCPU 查询入口
│   ├── config.rs                   # VM 配置加载和解析，文件访问走 host::fs
│   ├── fdt/                        # guest FDT 解析/生成
│   ├── hvc.rs                      # hypercall 分发
│   ├── images/                     # guest image 加载
│   ├── ivc.rs                      # inter-VM communication
│   ├── timer.rs                    # 迁移期保留；最终并入 host::timer
│   ├── vcpus.rs                    # vCPU lifecycle，task/irq 操作走 host
│   └── vm_list.rs                  # VM registry
└── shell/                          # 管理 shell，输出/输入能力走 host
    └── ...
```

`os/axvisor/src/hal` 的能力迁入 `host` 后，不再作为业务层入口。是否删除该目录可以放到迁移末期决定；原则是业务层不再依赖它。

### 7. 引用规范

`main.rs`：

```rust
mod host;
mod shell;
mod task;
mod vmm;

fn main() {
    print_logo();

    host::platform::enable_virtualization_on_all_cpus();

    vmm::init();
    vmm::start();

    shell::console_init();
}
```

`vmm/mod.rs`：

```rust
use crate::host::{
    self,
    task as host_task,
};
```

`vmm/vcpus.rs`：

```rust
use crate::host::{
    cpu::HostCpuMask,
    irq,
    task as host_task,
};
```

`vmm/config.rs`：

```rust
use crate::host::fs;

let content = fs::read_to_string(path)?;
```

业务层不再出现：

```rust
use ax_api;
use ax_hal;
use ax_task;
use ax_alloc;
use ax_std::os::arceos::{api, modules};
```

例外只能出现在 `os/axvisor/src/host` 内部，或明确标注为短期迁移例外。

### 8. Cargo 依赖方向

`os/axvisor` 保留 `ax-std` 作为 ArceOS 统一 facade 依赖：

```toml
ax-std = { workspace = true, features = [
    "paging",
    "irq",
    "multitask",
    "task-ext",
    "smp",
    "hv",
    "buddy-slab",
] }
```

Axvisor feature 可继续通过 `ax-std` 聚合普通 ArceOS 能力：

```toml
fs = ["ax-std/fs"]
dyn-plat = ["ax-std/plat-dyn", "dep:axplat-dyn"]
```

底层 driver/platform feature 可以继续由 Axvisor build config 直接控制：

```toml
rockchip-soc = ["ax-driver/rockchip-soc"]
rockchip-sdhci = ["ax-driver/rockchip-sdhci", "ax-driver/rockchip-soc"]
```

`os/axvisor` 不应同时直接依赖 `ax-std` 和 `ax-api` 来做同一层能力调用。目标关系是：

```text
os/axvisor
    -> ax-std
        -> ax-api
```

而不是：

```text
os/axvisor
    -> ax-std
    -> ax-api
```

若短期因 trait、宏、类型路径限制必须保留直接依赖，应限制在 `host` 内部，并标记为迁移例外。

### 9. 迁移步骤

1. 明确 `ax_std::os::arceos::{api, modules}` 的 facade 语义，补充 `ax-std` 文档。
2. 新建 `os/axvisor/src/host`，先复制 `hal` 能力，不删除旧代码。
3. 新增 `host/console.rs`，将启动 banner 和必要交互输出收敛到 `host::console`。
4. 新增 `host/fs.rs`，将 `vmm/config.rs` 的文件访问收敛到 `host::fs`。
5. 迁移 `hal/impl_memory.rs` 到 `host/api_impl/memory.rs`，底层内存能力放 `host/memory.rs`。
6. 迁移 `hal/impl_time.rs` 到 `host/api_impl/time.rs`，底层时间能力放 `host/time.rs`。
7. 迁移 `hal/impl_vmm.rs` 到 `host/api_impl/vmm.rs`，当前 task/vCPU 查询走 `host/task.rs`。
8. 迁移 `hal/impl_host.rs` 到 `host/api_impl/host.rs`。
9. 迁移 `hal/mod.rs` 中的 `AxMmHalImpl`、per-cpu、enable virtualization 到 `host/paging.rs`、`host/percpu.rs`、`host/platform.rs`。
10. 迁移 `hal/arch/*` 到 `host/arch/*`。
11. 修改 `main.rs`：`hal::enable_virtualization()` 改为 `host::platform::enable_virtualization_on_all_cpus()`。
12. 修改 `vmm/vcpus.rs`：所有 `ax_task`、`ax_hal` 调用改为 `host::task`、`host::irq`。
13. 修改 `task.rs`：底层 `TaskExt` 相关引用尽量来自 `host::task` re-export，或将 task 扩展 trait 迁入 `host/task.rs`。
14. 待 `src/hal` 无剩余职责后，删除或保留为空壳兼容层；原则上不再作为业务层入口。
15. 加入边界检查，禁止业务层重新直接依赖底层 ArceOS 模块。

边界检查示例：

```bash
rg -n "ax_api|ax_hal|ax_task|ax_alloc|ax_std::os::arceos" \
  os/axvisor/src/main.rs os/axvisor/src/vmm os/axvisor/src/shell
```

期望结果为空，例外统一放入 `os/axvisor/src/host`。

## 遗留问题

### 1. `ax_std::os::arceos::modules` 的稳定性边界

`modules` 是 escape hatch，不是普通应用 API。需要进一步明确：

- 哪些底层 crate 可以 re-export；
- 每个 re-export 受哪些 feature 控制；
- 哪些类型可以进入 `prelude`；
- 哪些 API 应上移到 `ax-api`，避免复杂系统长期依赖 Tier C。

### 2. `axvisor_api` 是否需要拆分

当前保留 `axvisor_api` 是合理的，但它可能逐渐变成“大而全”的 hypervisor capability crate。后续可以评估是否拆成：

```text
virt-memory-api
virt-time-api
virt-irq-api
virt-vmm-api
virt-host-api
```

短期不建议删除 `axvisor_api`，否则 virtualization 组件会直接耦合 ArceOS 或陷入复杂的泛型注入。

### 3. Rust native `std` 支持

`ax-std` 的 `ax_std::os::arceos` 形态应尽量贴近未来：

```rust
std::os::arceos
```

但真正实现 `target_os = "arceos"` 仍需要额外工作：

- Rust target spec；
- libstd platform adaptation layer；
- syscall/ABI 约定；
- thread、fs、net、time、env、process 等 std API 对接；
- panic、TLS、unwind、alloc、start/runtime 入口；
- cargo/build-std 集成。

当前方案只是为这个方向预留路径，不直接完成 native std。
