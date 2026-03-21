# `axstd` 技术文档

> 路径：`os/arceos/ulib/axstd`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 用户库层
> 版本：`0.3.0-preview.3`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`axstd` 的核心定位是：ArceOS user library with an interface similar to rust std

## 1. 架构设计分析
- 目录角色：ArceOS 用户库层
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：主要通过 `alloc`、`alloc-buddy`、`alloc-level-1`、`alloc-slab`、`alloc-tlsf`、`bus-mmio`、`bus-pci`、`defplat`、`display`、`dma` 等（另有 31 个 feature） 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `Instant`、`Result`、`Output`。
- 设计重心：该 crate 位于应用接口边界，重点是把底层模块能力包装成更接近 Rust `std` / libc 语义的用户态或应用开发接口。

### 1.1 内部模块划分
- `macros`：Standard library macros Prints to the standard output. Equivalent to the [println!] macro except that a newline is not printed at the end of the message. [println!]: crate::println
- `env`：Inspection and manipulation of the process’s environment
- `io`：Traits, helpers, and type definitions for core I/O functionality
- `os`：OS-specific functionality. ArceOS-specific definitions
- `process`：A module for working with processes. Since ArceOS is a unikernel, there is no concept of processes. The process-related functions will affect the entire system, such as [exit] wil…
- `sync`：Useful synchronization primitives
- `thread`：Native threads
- `time`：Temporal quantification

### 1.2 核心算法/机制
- 进程生命周期、资源共享与回收
- socket 状态机与连接管理

## 2. 核心功能说明
- 功能定位：ArceOS user library with an interface similar to rust std
- 对外接口：从源码可见的主要公开入口包括 `current_dir`、`set_current_dir`、`exit`、`yield_now`、`sleep`、`sleep_until`、`available_parallelism`、`now`、`Instant`。
- 典型使用场景：面向应用开发者提供 `std`/libc 风格接口，是应用与底层 `arceos_api`/内核模块之间的主要边界层。
- 关键调用链示例：该 crate 没有单一固定的初始化链，常由应用按线程、时间、I/O、文件系统和网络等模块分别接入。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["axstd"]
    current --> arceos_api["arceos_api"]
    current --> axerrno["axerrno"]
    current --> axfeat["axfeat"]
    current --> axio["axio"]
    current --> kspin["kspin"]
    current --> lazyinit["lazyinit"]
    arceos_affinity["arceos-affinity"] --> current
    arceos_helloworld["arceos-helloworld"] --> current
    arceos_helloworld_myplat["arceos-helloworld-myplat"] --> current
    arceos_httpclient["arceos-httpclient"] --> current
    arceos_httpserver["arceos-httpserver"] --> current
    arceos_irq["arceos-irq"] --> current
    arceos_memtest["arceos-memtest"] --> current
    arceos_parallel["arceos-parallel"] --> current
```

### 3.1 直接与间接依赖
- `arceos_api`
- `axerrno`
- `axfeat`
- `axio`
- `kspin`
- `lazyinit`

### 3.2 间接本地依赖
- `arm_pl011`
- `arm_pl031`
- `axalloc`
- `axallocator`
- `axbacktrace`
- `axconfig`
- `axconfig-gen`
- `axconfig-macros`
- `axcpu`
- `axdisplay`
- `axdma`
- `axdriver`
- 另外还有 `55` 个同类项未在此展开

### 3.3 被依赖情况
- `arceos-affinity`
- `arceos-helloworld`
- `arceos-helloworld-myplat`
- `arceos-httpclient`
- `arceos-httpserver`
- `arceos-irq`
- `arceos-memtest`
- `arceos-parallel`
- `arceos-priority`
- `arceos-shell`
- `arceos-sleep`
- `arceos-wait-queue`
- 另外还有 `2` 个同类项未在此展开

### 3.4 间接被依赖情况
- 当前未发现更多间接消费者，或该 crate 主要作为终端入口使用。

### 3.5 关键外部依赖
- `lock_api`
- `spin`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
axstd = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# axstd = { path = "os/arceos/ulib/axstd" }
```

### 4.2 初始化流程
1. 将该 crate 视作应用接口层，先明确是走 `axstd` 风格还是 libc/POSIX 风格接入。
2. 根据应用所需能力开启 feature，并确认与 `arceos_api`/系统镜像配置保持一致。
3. 通过最小应用或示例程序验证线程、时间、I/O、文件系统或网络接口的语义是否正确。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`current_dir`、`set_current_dir`、`exit`、`yield_now`、`sleep`、`sleep_until`、`available_parallelism`、`now` 等（另有 4 项）。
- 上下文/对象类型通常从 `Instant` 等结构开始。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。

### 5.2 单元测试重点
- 建议覆盖 std/libc 风格包装层的语义映射、错误码转换和 feature 分支。

### 5.3 集成测试重点
- 建议用最小应用、示例程序和系统镜像运行验证线程、I/O、时间、文件系统和网络接口语义。

### 5.4 覆盖率要求
- 覆盖率建议：对外暴露的高层 API 需要稳定覆盖；与底层子系统交互的关键路径应至少有一条端到端验证。

## 6. 跨项目定位分析
### 6.1 ArceOS
`axstd` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 用户库层。

### 6.2 StarryOS
当前未检测到 StarryOS 工程本体对 `axstd` 的显式本地依赖，若参与该系统，通常经外部工具链、配置或更底层生态间接体现。

### 6.3 Axvisor
`axstd` 不在 Axvisor 目录内部，但被 `axvisor` 等 Axvisor crate 直接依赖，说明它是该系统的共享构件或底层服务。
