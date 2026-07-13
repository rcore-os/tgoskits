# ArceOS 用户库边界重构方案

## 1. 目标

彻底删除 `os/arceos/api/`，取消 ArceOS 公共 API 聚合层，使：

- `axstd` 成为 Rust 应用唯一的用户库入口；
- `axlibc` 成为 C 应用唯一的用户库入口；
- `axstd` 与 `axlibc` 完全解耦，二者不得互相依赖；
- 上层系统组件直接依赖 `ax-runtime`、`ax-task`、`ax-fs-ng`、`ax-net` 等底层模块；
- 不再通过 `ax-api`、`ax-posix-api` 或其他用户 API 聚合 crate 转发能力；
- 保持底层模块、runtime、驱动和文件系统实现的职责边界不变。

目标依赖关系：

```text
Rust Application -> axstd -> ax-runtime / ax-task / ax-fs-ng / ax-net / ax-hal / ...
C Application    -> axlibc -> ax-runtime / ax-task / ax-fs-ng / ax-net / ax-hal / ...
```

`axstd` 与 `axlibc` 只共享底层内核模块，不共享用户库实现、POSIX facade 或 API 聚合 crate。

ArceOS native application 和系统级 OS/系统软件（StarryOS、Axvisor）是两类不同的构建对象。

`axstd` 是 Rust 系统软件和 Rust 应用都可以使用的 Rust `std` 风格基础库；`axlibc` 是面向 C 应用的 libc。StarryOS 和 Axvisor 可以使用 `axstd` 提供标准 Rust 接口，同时必须直接依赖 ArceOS runtime、内核模块、平台和驱动来实现系统专用能力。

当前实现尚未完全达到该目标：

- `starryos` 生产镜像直接依赖 `ax-std`，并通过 `use ax_std as _` 使用 Rust 标准库和链接/runtime glue；
- `starry-kernel` 的生产依赖主要是底层 ArceOS modules，但 kernel test 仍使用 `axstd` 作为测试支持；
- Axvisor 直接依赖并使用 `axstd` 的标准接口，包括 `ax_std::fs` 和 `ax_std::io`，同时通过 `ax_std::os::arceos::modules` 访问了本应直接依赖的底层模块；
- `axvm` 通过 `axstd` 访问 ArceOS host API、任务、定时器、HAL 和模块 facade；
- Starry LKM 和部分系统测试也仍然依赖 `axstd`。

因此，本方案中的“系统级软件直接组合底层模块”只针对系统专用能力，不意味着系统级 Rust 软件必须移除 `axstd`。目标是保留 `axstd` 的标准 Rust 接口，移除 `axstd::os::arceos::modules` 这类底层模块 facade，并让系统核心代码对特殊能力直接声明和使用对应模块。

```text
ArceOS native application
        │
        ├── Rust application -> axstd
        └── C application    -> axlibc
                                      │
                                      ▼
                              ArceOS runtime/modules
```

```text
系统级 OS / 系统软件
        │
        ├── 标准 Rust 接口 -> axstd
        │
        ├── StarryOS -> Starry kernel + Linux syscall ABI
        │                         │
        │                         ▼
        │                  ArceOS runtime/modules
        │
        └── Axvisor  -> hypervisor/platform/guest management
                                  │
                                  ▼
                          ArceOS runtime/modules
```

StarryOS 中的 Linux 用户程序是系统级 OS 所承载的 guest/user workload，不是与 ArceOS native application 并列的第三类 TGOSKits 构建对象：

```text
Starry rootfs ELF
        │
        ▼
Linux/musl libc + syscall ABI
        │
        ▼
Starry syscall dispatcher
        │
        ▼
Starry kernel + ArceOS modules
```

`axstd` 的标准 Rust 接口可以被 ArceOS native application、StarryOS 和 Axvisor 使用；`axlibc` 面向 C 应用和 C 运行时。二者都不定义 StarryOS 的 Linux syscall 接口或 Axvisor 的 hypervisor/guest 接口，系统专用部分仍由对应系统组件和底层模块负责。

## 2. 非目标

- 不引入 Linux 风格的 UAPI；
- 不新增 `ax-uapi` 或类似公共 ABI crate；
- 不通过新增内部共享用户库 crate 规避删除 `ax-api` 和 `ax-posix-api`；
- 不改变底层模块核心语义；
- 不通过复制底层内核实现来实现解耦。

### 2.1 兼容策略

本次变更是 breaking change，不提供旧 API 的兼容阶段：

- 不保留 `os/arceos/api/` 作为兼容目录；
- 不保留 `std::os::arceos::api` 旧路径；
- 不增加 deprecated alias、转发 crate 或旧 feature 前缀；
- 所有应用、系统组件、测试和文档在同一迁移中切换到新入口；
- 迁移完成后，旧路径和旧 crate 必须从 workspace 和源码中删除。

对于 ArceOS native application，当前单镜像模型不需要 Linux 风格 UAPI。

系统级 OS 需要单独处理自己的边界：StarryOS 已经拥有独立用户地址空间、ELF 用户程序、syscall trap、syscall number 和 Linux 兼容 syscall handler，因此 Starry 的 Linux syscall ABI 是 Starry 用户态和 Starry kernel 之间的兼容边界；Axvisor 则主要处理 hypervisor、host/platform 和 guest 之间的虚拟化边界。上述边界都不属于 `axstd` 或 `axlibc`。

如果未来需要整理 Starry 的本地 ABI 定义，应放在 Starry 的 syscall/ABI 层，而不是 `os/arceos/ulib/axstd` 或 `os/arceos/ulib/axlibc`。

## 3. 当前问题

当前结构为：

```text
axstd -> ax-api ---------+
                         +-> ArceOS modules
axlibc -> ax-posix-api --+
```

问题包括：

1. `ax-api` 同时包含 Rust 原生接口和底层模块 re-export；
2. `ax-posix-api` 同时承担 POSIX 实现和公共 API facade；
3. `axstd` 的 `std-compat` 依赖 `ax-posix-api`；
4. `std::os::arceos::api` 暴露额外 facade；
5. Axvisor、StarryOS 等系统组件容易间接依赖 `axstd`；
6. 用户库接口和内核模块接口没有清晰边界；
7. API feature、runtime feature 和底层模块 feature 被多层转发；
8. `os/arceos/api` 中的 feature 大量只是把上层选择转发到更底层 crate，本身没有独立行为。

最终结构必须是：

```text
axstd ---------------> ArceOS modules
axlibc --------------> ArceOS modules
```

## 4. 依赖约束

### 4.1 `axstd`

`axstd` 面向 Rust 应用，提供类似 Rust `std` 的接口，可以提供 `std::os::arceos` 扩展，但必须：

- 直接依赖底层 ArceOS modules；
- 不依赖 `axlibc`、`ax-api`、`ax-posix-api`；
- 不通过 C libc 实现普通 Rust API；
- 不提供 C/POSIX compatibility layer；Rust 应用只使用 `axstd` 的 Rust API。

### 4.2 `axlibc`

`axlibc` 面向 C 应用，提供 libc 函数、C ABI 和公开 C headers，但必须：

- 直接依赖底层 ArceOS modules；
- 不依赖 `axstd`、`ax-api`、`ax-posix-api`；
- 自己拥有 POSIX 函数实现；
- 自己生成和维护 C ABI 类型布局。

### 4.3 底层模块

`ax-runtime`、`ax-task`、`ax-fs-ng`、`ax-net`、`ax-sync`、`ax-hal` 等模块：

- 不得依赖 `axstd` 或 `axlibc`；
- 不得依赖任何用户库；
- 只提供内核、runtime、设备和基础设施能力。

### 4.4 系统组件

StarryOS、Axvisor、LKM 和其他系统组件：

- 不得通过用户库间接获取底层模块；
- 根据实际职责直接依赖 `ax-runtime`、`ax-task`、`ax-mm`、`ax-driver` 等 crate；
- Rust 系统代码可以使用 `axstd` 提供标准 Rust 接口；C 应用或 C 系统组件使用 `axlibc`；
- 系统专用能力必须直接依赖对应的 ArceOS modules，不得通过用户库 re-export 底层模块。

## 5. 删除内容

删除：

```text
os/arceos/api/arceos_api/
os/arceos/api/arceos_posix_api/
os/arceos/api/feature/
```

以及以下 workspace crate、dependency 和引用：

```text
ax-api
ax-posix-api
ax-feat
```

如果 `ax-feat` 已经在 PR #1513 中删除，则不恢复。

## 6. `ax-api` 迁移方案

### 6.1 系统、时间和内存

将 `ax-api` 中的以下内容迁入 `axstd` 的内部 ArceOS 实现模块：

```text
ax_get_cpu_num
ax_terminate
ax_monotonic_time
ax_wall_time
ax_alloc
ax_dealloc
ax_alloc_coherent
ax_dealloc_coherent
```

当前目标目录：

```text
os/arceos/ulib/axstd/src/
├── os.rs
└── os/arceos.rs
```

`os/arceos.rs` 组织 `sys`、`time`、`task`、`stdio`、`display` 等 ArceOS 扩展，并保留只供 `axstd` 自身使用的文件系统和网络适配。Rust 应用通过 `std::process`、`std::thread`、`std::time`、`std::alloc` 使用标准能力；不再为 `ax_alloc`、`ax_dealloc`、DMA 分配建立 `std::os::arceos::mem` 或 `dma` facade。

不得继续提供：

```rust
std::os::arceos::api
```

### 6.2 任务和同步

`AxTaskHandle`、`AxWaitQueueHandle`、`AxRawMutex` 等类型不再由 `ax-api` 提供。

- Rust 应用所需能力迁入 `axstd::thread`、`axstd::sync`；
- `WaitQueue` 等 ArceOS 特有能力放入 `std::os::arceos::task`；
- 类型定义直接位于 `axstd`，不再跨 crate 作为 facade 类型导出；
- `axlibc` 使用自己的 pthread 和等待队列适配，不复用 `axstd` 类型。

### 6.3 文件系统

`AxFileHandle`、`AxDirHandle`、`AxOpenOptions`、`AxFileAttr`、`AxDirEntry` 等内容迁入 `axstd::fs` 内部实现。

- `axstd::fs::File` 持有仅在 `axstd` 内可见的后端句柄，后端句柄封装底层 `ax-fs-ng` 对象；
- `OpenOptions` 使用 Rust 风格接口；
- 底层句柄类型保持私有；
- 文件系统错误转换为 `std::io::Error`；
- 不再暴露 `AxFileHandle` 等 facade 类型。

### 6.4 网络

TCP/UDP 的公开接口位于 `os/arceos/ulib/axstd/src/net/`。`TcpStream`、`TcpListener`、`UdpSocket` 持有仅在 `axstd` 内可见的后端句柄；后端句柄封装底层 `ax-net` 对象，错误转换为 `std::io::Error`。

### 6.5 显示

显示能力不属于 Rust `std` 标准 API。需要时提供：

```rust
std::os::arceos::display
```

该接口直接依赖 `ax-display`，不得经过 `ax-api`。

### 6.6 删除模块 facade

删除：

```rust
std::os::arceos::modules
```

该 facade 删除后不再有消费者。Axvisor、StarryOS 等复杂系统必须直接依赖 `ax-hal`、`ax-task`、`ax-runtime`、`ax-mm`、`ax-net` 等底层模块；它们仍可以同时使用 `axstd` 提供的标准 Rust 接口。

## 7. `ax-posix-api` 迁移方案

### 7.1 POSIX 实现迁入 `axlibc`

将以下实现迁入 `axlibc`：

```text
src/imp/fd_ops.rs
src/imp/fs.rs
src/imp/io.rs
src/imp/io_mpx/
src/imp/net.rs
src/imp/pipe.rs
src/imp/pthread/
src/imp/resources.rs
src/imp/sys.rs
src/imp/task.rs
src/imp/time.rs
src/utils.rs
```

目标结构：

```text
os/arceos/ulib/axlibc/src/
├── backend/                # C ABI 适配后的 ArceOS/POSIX 后端
│   ├── fd_table.rs
│   ├── io_multiplex/
│   ├── process.rs
│   ├── resource.rs
│   └── system.rs
├── fd.rs
├── fs.rs
├── io.rs
├── io_multiplex.rs
├── net.rs
├── pipe.rs
├── pthread.rs
├── resource.rs
├── system.rs
├── time.rs
└── utils.rs
```

外层模块仅导出 C ABI；`backend/` 直接调用 `ax-fs-ng`、`ax-net`、`ax-task`、`ax-sync`、`ax-runtime`、`ax-alloc` 和 `ax-hal`。这不是新的公共 API 层。

不再存在 `sys_*` 公共 crate。`sys_*` 可以作为 `axlibc` 内部函数名，但不得跨 crate 暴露。

### 7.2 C ABI 和 headers

`axlibc` 独立拥有：

```text
os/arceos/ulib/axlibc/include/
os/arceos/ulib/axlibc/ctypes.h
os/arceos/ulib/axlibc/build.rs
```

迁移内容包括 C 类型生成、pthread mutex layout、`stat`、`pollfd`、`epoll_event`、`sockaddr`、POSIX 常量、errno 映射和 C 函数导出。

删除 `ax-posix-api/build.rs`。

### 7.3 Rust/C ABI 边界

`axstd` 不提供 `std-compat` feature，也不保留 `axstd/src/os/libc_compat.rs`。Rust 应用只使用 `axstd` 的 Rust API；C ABI、POSIX 类型和兼容符号均由 `axlibc` 提供。若将来确有 Rust 侧链接符号需求，应在 `axstd` 中以 native Rust 实现单独设计，并直接调用底层模块，不能重新依赖 `axlibc` 或 POSIX 实现 crate。

## 8. Feature 迁移

### 8.1 `axstd`

`axstd` 只拥有 Rust 用户库接口 feature，并直接选择底层模块和 runtime：

```toml
[features]
default = ["alloc", "tls"]

alloc = ["dep:ax-alloc", "ax-io/alloc"]
multitask = ["ax-task/multitask", "ax-sync/multitask", "ax-runtime/multitask"]
fs = ["dep:ax-fs-ng", "ax-runtime/fs"]
net = ["dep:ax-net", "ax-runtime/net"]
display = ["dep:ax-display", "ax-runtime/display"]
```

`ax-runtime` 没有 `alloc` feature，因此分配器由 `axstd`/`axlibc` 直接选择。`axstd` 可以保留直接映射到 `ax-driver` 的设备 feature；这些 feature 具有真实的依赖与驱动行为，不是被删除 API crate 的空转发。

禁止出现：

```toml
ax-api/*
ax-posix-api/*
axlibc/*
```

### 8.2 `axlibc`

`axlibc` 只拥有 C/POSIX 用户库 feature：

```toml
[features]
default = []

alloc = ["dep:ax-alloc"]
multitask = ["ax-task/multitask", "ax-sync/multitask", "ax-runtime/multitask"]
fs = ["dep:ax-fs-ng", "ax-runtime/fs", "fd"]
net = ["dep:ax-net", "ax-runtime/net", "fd"]
fd = ["alloc", "dep:scope-local"]
pipe = ["fd"]
poll = ["fd"]
select = ["fd"]
epoll = ["fd"]
```

禁止出现：

```toml
ax-api/*
ax-posix-api/*
axstd/*
```

### 8.3 Runtime

`ax-runtime` 只负责系统启动、全局 runtime 初始化、设备注册、中断注册、文件系统初始化、网络栈初始化以及 SMP/调度器装配。

`ax-runtime` 不负责导出用户 API，也不得依赖 `axstd` 或 `axlibc`。

### 8.4 删除 API 层 feature 转发

删除 `os/arceos/api/` 后，feature 关系从：

```text
应用
  -> axstd / axlibc
  -> ax-api / ax-posix-api
  -> ax-runtime
  -> ax-task / ax-fs-ng / ax-net / ax-hal / ...
```

简化为：

```text
ArceOS native Rust application
  -> axstd feature
  -> ax-runtime + 底层模块 feature

ArceOS native C application
  -> axlibc feature
  -> ax-runtime + 底层模块 feature

StarryOS / Axvisor
  -> 系统组件自身 feature
  -> ax-runtime + 底层模块 feature
```

删除后各层 feature 的所有权如下：

| 层级 | 负责内容 | 不负责内容 |
| --- | --- | --- |
| `axstd` | Rust 标准接口是否编译、Rust 应用能力选择、对 runtime/module 的直接 feature 映射 | C ABI、POSIX 实现、底层模块 re-export |
| `axlibc` | C/POSIX 接口是否编译、C ABI、C 应用能力选择、对 runtime/module 的直接 feature 映射 | Rust `std` 接口、`axstd` 实现 |
| `ax-runtime` | 启动、全局状态、设备注册、文件系统/网络/SMP 等系统装配 | Rust/C 用户 API facade |
| `ax-task`、`ax-fs-ng`、`ax-net`、`ax-hal` 等模块 | 本 crate 的实现选择和局部 feature | 上层用户库 feature 聚合 |
| StarryOS / Axvisor | 系统专用能力和平台/guest/hypervisor 装配 | 通过用户库 re-export 底层模块 |

`axstd` 和 `axlibc` 仍然可以保留同名 feature，例如 `fs`、`net`、`multitask`，但这些 feature 必须在各自 crate 中明确映射到实际依赖：

```toml
# axstd 或 axlibc 中的合法映射
fs = ["ax-runtime/fs", "dep:ax-fs-ng"]
net = ["ax-runtime/net", "dep:ax-net"]
multitask = ["ax-runtime/multitask", "ax-task/multitask"]
```

这里的映射是用户库选择能力并装配 runtime 的必要组合，不是另一个公共 API crate 的无行为转发。`ax-api/*`、`ax-posix-api/*` 和 `ax-feat/*` 不得再出现在任何 feature 定义中。

feature 迁移必须满足：

- 每个底层实现 feature 由实际拥有该实现的 crate 定义；
- runtime 初始化 feature 由 `ax-runtime` 定义；
- Rust 用户接口 feature 由 `axstd` 定义；
- C/POSIX 用户接口 feature 由 `axlibc` 定义；
- StarryOS 和 Axvisor 的系统装配 feature 由自身 crate 定义；
- 不保留没有 `cfg`、依赖或行为差异的空转发 feature；
- 不通过新增新的 feature 聚合 crate 重新制造被删除的层级。

### 8.5 系统级软件与 `axstd` 的依赖菱形

系统级 Rust 软件同时依赖 `axstd` 和底层模块是允许的。例如：

```text
Axvisor / StarryOS
        ├── axstd ─────── ax-task / ax-hal / ax-runtime / ...
        └── ax-task / ax-hal / ax-runtime / ...
```

这在 Cargo 依赖图中形成依赖菱形，但不代表存在两套实现，也不代表系统代码存在两条等价的 API 调用路径。Cargo 会对相同 workspace package 进行依赖解析和 feature 合并；架构上真正需要禁止的是通过 `axstd` 的底层模块 re-export 访问系统能力。

允许的调用边界：

```text
标准 Rust 接口：
    Axvisor / StarryOS -> axstd::fs / axstd::io / axstd::thread / axstd::sync / ...

系统专用接口：
    Axvisor / StarryOS -> ax-task / ax-hal / ax-mm / ax-runtime / ax-driver / ...
```

禁止的调用边界：

```text
Axvisor / StarryOS -> axstd::os::arceos::modules::ax-task
Axvisor / StarryOS -> axstd::os::arceos::modules::ax-hal
```

系统级 crate 的 manifest 必须显式声明其直接使用的底层模块。这样可以让系统专用依赖、feature 和硬件能力在系统 crate 自身的 `Cargo.toml` 中可审计；`axstd` 只负责它实际提供的标准 Rust 接口及其必要依赖。

系统级 crate 的 feature 处理规则：

- `axstd` feature 只启用系统实际使用的标准 Rust 接口，例如 `fs`、`multitask`、`irq`；
- 系统专用 feature 由 StarryOS/Axvisor 自身定义，并直接映射到底层模块；
- 不为了访问底层模块而额外打开 `axstd` feature；
- 不因为 Cargo 依赖图中存在两条到同一底层 crate 的路径，就复制实现或增加新的 facade；
- 使用 `cargo tree -e features -p axvisor`、`cargo tree -e features -p starryos` 检查 feature 合并结果；
- 使用 `rg` 审计，确保系统代码没有 `axstd::os::arceos::modules` 或 `std::os::arceos::api` 引用。

## 9. 上层消费者迁移

### 9.1 Rust 应用

普通 Rust 应用继续依赖 `ax-std`，优先使用：

```rust
std::fs
std::io
std::net
std::thread
std::sync
std::time
```

ArceOS 特有扩展使用 `std::os::arceos`，删除全部 `std::os::arceos::api` 引用。

### 9.2 ArceOS 测试

- 能转换为标准库接口的测试，转换为标准库接口；
- 纯任务、等待队列测试直接放入 `ax-task` 或系统测试；
- 显示测试迁移到 `std::os::arceos::display`；
- 不为了维持旧路径而重新增加 API facade。

### 9.3 Axvisor

当前 Axvisor 直接依赖 `axstd`。例如，`os/axvisor/src/manager.rs` 使用 `ax_std::fs` 和 `ax_std::io`，`os/axvisor/src/main.rs` 使用 `ax_std as _`，而 `axvm` 通过 `ax_std::os::arceos::modules` 访问底层能力。

目标状态不是移除 Axvisor 对 `axstd` 的依赖。Axvisor 可以继续通过 `axstd` 使用标准 Rust 接口，例如文件、IO、线程和同步抽象；但系统专用能力不得通过 `axstd` 间接访问底层模块，而是按实际使用直接依赖：

```text
ax-hal
ax-runtime
ax-task
ax-sync
ax-mm
ax-alloc
ax-driver
```

Axvisor 的 host、hypervisor、platform 和 guest management 代码可以使用 `axstd` 的标准 Rust 接口，但不得使用 `axstd::os::arceos::modules` 作为底层模块 facade。

### 9.4 StarryOS 和 LKM

StarryOS kernel、LKM 和系统服务：

- 不依赖 `ax-api` 或 `ax-posix-api`；
- 不通过 `axstd::os::arceos::modules` 访问底层模块；
- 直接声明并配置所需底层 feature；
- Rust 系统代码和用户应用都可以使用 `axstd` 的标准 Rust 接口；
- 内核组件直接依赖内核模块。

当前 StarryOS 仍有两类需要区分的使用：`starryos` 镜像入口使用 `axstd` 的标准 Rust 接口和链接/runtime glue；kernel test、LKM 和部分测试代码使用 `axstd` 作为测试或应用支持。需要迁移的是通过 `axstd::os::arceos::modules` 获取底层能力的路径，而不是所有 `axstd` 依赖。上述使用也不代表 Starry Linux rootfs 用户程序使用 `axstd`。

Starry rootfs 中的 `/bin/sh`、busybox、Python 和测试程序属于 Linux 用户态程序，不属于 ArceOS native application。它们通过 musl/glibc 提供的 Linux 用户库接口发起 syscall，不能把 `axstd` 或当前直接调用 ArceOS modules 的 `axlibc` 当作 Starry 用户态 libc。

Starry 的实际链路为：

```text
rootfs ELF
    -> musl/glibc 用户库
    -> syscall 指令和 Linux syscall ABI
    -> ax_runtime 用户态 trap / UserContext
    -> starry-kernel/src/syscall/mod.rs
    -> starry-kernel/src/syscall/{fs,net,mm,task,...}
    -> Starry process/file/mm 语义层
    -> ax-runtime、ax-task、ax-mm、ax-fs-ng、ax-net、ax-hal
```

Starry syscall 层的职责包括：

- 读取和校验用户寄存器、syscall number 及参数；
- 通过用户地址空间安全访问用户内存；
- 将 Linux ABI 参数转换为 Starry 内部类型；
- 实现 Linux 的进程、线程、文件、网络、内存、信号和 IPC 语义；
- 将 `AxError` 或内部错误转换为 Linux errno；
- 将返回值写回用户态 `UserContext`。

Starry 的 UAPI/ABI 定义包括 syscall number、`struct stat`、`epoll_event`、`sockaddr`、ioctl 编号、flags、signal/futex 类型和其他用户态可见布局。当前相关定义来自 Linux/musl 用户态 sysroot、`linux-raw-sys`、`syscalls::Sysno` 以及 Starry 的兼容实现；它们不能被归入 `axstd` 或 `axlibc`。

当前 `os/StarryOS/starryos/src/main.rs` 中的 `use ax_std as _;` 表示 Starry 内核镜像使用 `axstd` 提供的标准 Rust 接口和链接/runtime glue，不表示 rootfs 用户程序使用 `axstd`。该依赖可以保留；需要迁移的是系统专用能力对 `axstd::os::arceos::modules` 的使用，应改为 Starry kernel 或系统组件直接依赖对应的底层模块。Starry kernel 的测试、LKM 和系统工具也可以继续使用 `axstd` 的标准接口。

## 10. Workspace、目录和文档

修改根目录 `Cargo.toml`：

- 删除 `os/arceos/api/*` workspace members；
- 删除 `ax-api` 和 `ax-posix-api` workspace dependencies；
- 删除所有指向 `os/arceos/api` 的路径依赖；
- 重新生成 `Cargo.lock`。

删除：

```text
os/arceos/api/
```

更新：

```text
os/arceos/README.md
os/arceos/README_CN.md
.github/MAINTAINERS.md
docs/docs/architecture/arceos.md
docs/docs/architecture/overview.md
docs/docs/development/arceos.md
docs/docs/development/components.md
docs/docs/components/layers.md
docs/docs/components/overview.md
docs/docs/components/crates/ax-std.md
docs/docs/components/crates/ax-libc.md
```

删除或重写：

```text
docs/docs/components/crates/ax-api.md
docs/docs/components/crates/arceos-api.md
docs/docs/components/crates/ax-posix-api.md
```

清理以下引用：

```text
os/arceos/api/
ax-api
ax-posix-api
std::os::arceos::api
ax-api/src
arceos_posix_api/src
```

## 11. 实施顺序

### 阶段一：建立基线

```bash
cargo metadata --no-deps
cargo tree -i ax-api --workspace
cargo tree -i ax-posix-api --workspace
rg -n 'ax-api|ax-posix-api|arceos_api|arceos_posix_api|std::os::arceos::api|ax_posix_api' --glob '!target'
```

记录当前 axstd、axlibc、ArceOS Rust/C test-suit、StarryOS 和 Axvisor 的可用构建结果。

### 阶段二：迁移 `axstd`

- 将 `ax-api` 使用迁入 `axstd`；
- 确认 Rust/C ABI 边界：移除旧 compatibility layer，不再以 `axstd` 提供 C/POSIX 符号；
- 删除 `std::os::arceos::api`；
- 更新 Rust 应用和测试；
- 验证 `axstd` 不再依赖任何用户 API crate。

### 阶段三：迁移 `axlibc`

- 将 `ax-posix-api` 实现迁入 `axlibc`；
- 将 `build.rs`、`ctypes.h` 和 ABI 生成逻辑迁入 `axlibc`；
- 更新 C 函数实现；
- 验证 `axlibc` 不依赖 `axstd`、`ax-api` 或 `ax-posix-api`。

### 阶段四：迁移系统组件

- 修改 Axvisor；
- 修改 StarryOS；
- 修改 LKM；
- 修改 virtualization；
- 修改 test-suit；
- 清理所有底层模块 facade 使用。

### 阶段五：删除旧 crate

确认没有代码依赖后：

- 删除 `os/arceos/api/arceos_api`；
- 删除 `os/arceos/api/arceos_posix_api`；
- 删除 workspace 配置、文档和维护者路径；
- 重新生成 `Cargo.lock`。

### 阶段六：审计

```bash
rg -n 'ax-api|ax-posix-api|arceos_api|arceos_posix_api|os/arceos/api|std::os::arceos::api' --glob '!target'
```

结果只允许保留必要的历史迁移说明，不能存在代码、manifest、构建脚本或新架构文档引用。

## 12. 验证要求

### 12.1 依赖验证

```bash
cargo tree -i ax-api --workspace
cargo tree -i ax-posix-api --workspace
```

两个命令均应失败并提示 crate 不存在。

```bash
cargo tree -p ax-std
cargo tree -p ax-libc
```

`ax-std` 的依赖树不得出现 `ax-api`、`ax-posix-api`、`ax-libc`；`ax-libc` 的依赖树不得出现 `ax-api`、`ax-posix-api`、`ax-std`。

### 12.2 编译验证

至少验证：

- axstd 默认、alloc、multitask、fs、net；
- axlibc 默认、multitask、fs、net、fd/poll/select/epoll；
- StarryOS；
- Axvisor；
- ArceOS Rust test-suit；
- ArceOS C test-suit。

ArceOS、StarryOS 和 Axvisor 优先使用 `cargo xtask` 命令验证。

### 12.3 Clippy 和格式化

```bash
cargo fmt --all
cargo xtask clippy --package ax-std
cargo xtask clippy --package ax-libc
```

对受影响的系统组件继续执行 targeted clippy，不得通过新增 `allow` 隐藏迁移问题。

### 12.4 C ABI 验证

重点验证：

- `pthread_mutex_t` 大小；
- SMP/非 SMP 布局；
- lockdep 布局；
- `stat`；
- `sockaddr`；
- `pollfd`；
- `epoll_event`；
- `timeval`；
- `rlimit`；
- `fd_set`；
- `pthread_t`；
- C 函数导出符号；
- errno 映射。

## 13. 验收标准

1. `os/arceos/api/` 不存在；
2. `ax-api` 不存在；
3. `ax-posix-api` 不存在；
4. `axstd` 不依赖 `axlibc`；
5. `axlibc` 不依赖 `axstd`；
6. `axstd` 和 `axlibc` 不依赖用户 API facade；
7. `std::os::arceos::api` 不存在；
8. 系统组件直接依赖底层模块；
9. `axstd` 不提供 C/POSIX compatibility layer，也不依赖 POSIX 实现 crate；
10. `axlibc` 独立拥有 C ABI 和 headers；
11. Rust 应用可以只依赖 `axstd`；
12. C 应用可以只依赖 `axlibc`；
13. 两条用户库链路可以独立编译；
14. 所有原有功能和 ABI 回归测试通过；
15. Starry rootfs 用户程序通过 Linux syscall ABI 进入 `starry-kernel`，不依赖 `axstd` 或直接调用 ArceOS modules；
16. Starry 和 Axvisor 的系统专用能力不通过 `axstd::os::arceos::modules` 获取，而是直接依赖对应底层模块；
17. Starry syscall/UAPI 兼容定义不归属于 `axstd` 或 `axlibc`；
18. feature 依赖图中不存在 `ax-api/*`、`ax-posix-api/*` 和 `ax-feat/*` 转发；
19. 每个保留 feature 都由实际拥有其行为的 crate 定义；
20. 文档、workspace、维护者配置和 CI 不再引用旧目录。

## 14. 最终原则

`axstd` 是 Rust 应用的标准库。

`axlibc` 是 C 应用的 libc。

二者不是彼此的封装，也不是同一个用户 API 的两种语言绑定。它们只共享 ArceOS 的底层 runtime、内核模块、设备驱动和基础设施；用户库层面的 API、实现、类型、错误转换、ABI 和 feature 均由各自独立维护。

ArceOS native application 当前不引入 Linux UAPI；它们通过 `axstd` 或 `axlibc` 直接使用 ArceOS runtime/modules。

StarryOS 已经拥有独立用户态、Linux syscall ABI 和对应的兼容边界。Starry 的 syscall/UAPI 定义应继续由 Starry 的 syscall/ABI 层维护，必要时再整理为独立的 Starry UAPI，而不是归入 `axstd` 或 `axlibc`。
