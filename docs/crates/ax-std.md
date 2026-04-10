# `ax-std` 技术文档

> 路径：`os/arceos/ulib/axstd`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 用户库层
> 版本：`0.5.0`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`ax-std` 的核心定位是：ArceOS user library with an interface similar to rust std

## 1. 架构设计分析
- 目录角色：ArceOS 用户库层
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：主要通过 `alloc`、`alloc-buddy`、`alloc-level-1`、`alloc-slab`、`alloc-tlsf`、`bus-mmio`、`bus-pci`、`defplat`、`display`、`dma` 等（另有 31 个 feature） 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `Instant`、`Result`、`Output`。

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
- 典型使用场景：主要作为仓库中的专用支撑 crate 被上层组件调用。
- 关键调用链示例：该 crate 没有单一固定的初始化链，通常由上层调用者按 feature/trait 组合接入。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["ax-std"]
    current --> ax-api["ax-api"]
    current --> ax_errno["ax-errno"]
    current --> ax-feat["ax-feat"]
    current --> axio["ax-io"]
    current --> ax_kspin["ax-kspin"]
    current --> ax_lazyinit["ax-lazyinit"]
    arceos_affinity["arceos-affinity"] --> current
    arceos_display["arceos-display"] --> current
    arceos_exception["arceos-exception"] --> current
    arceos_fs_shell["arceos-fs-shell"] --> current
    arceos_irq["arceos-irq"] --> current
    arceos_memtest["arceos-memtest"] --> current
    arceos_net_echoserver["arceos-net-echoserver"] --> current
    arceos_net_httpclient["arceos-net-httpclient"] --> current
```

### 3.1 直接与间接依赖
- `ax-api`
- `ax-errno`
- `ax-feat`
- `axio`
- `ax-kspin`
- `ax-lazyinit`

### 3.2 间接本地依赖
- `ax-arm-pl011`
- `ax-arm-pl031`
- `axaddrspace`
- `ax-alloc`
- `ax-allocator`
- `axbacktrace`
- `axconfig`
- `ax-config-gen`
- `ax-config-macros`
- `ax-cpu`
- `ax-display`
- `ax-dma`
- 另外还有 `60` 个同类项未在此展开

### 3.3 被依赖情况
- `arceos-affinity`
- `arceos-display`
- `arceos-exception`
- `arceos-fs-shell`
- `arceos-irq`
- `arceos-memtest`
- `arceos-net-echoserver`
- `arceos-net-httpclient`
- `arceos-net-httpserver`
- `arceos-net-udpserver`
- `arceos-parallel`
- `arceos-priority`
- 另外还有 `10` 个同类项未在此展开

### 3.4 间接被依赖情况
- 当前未发现更多间接消费者，或该 crate 主要作为终端入口使用。

### 3.5 关键外部依赖
- `lock_api`
- `spin`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
ax-std = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# ax-std = { path = "os/arceos/ulib/axstd" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`current_dir`、`set_current_dir`、`exit`、`yield_now`、`sleep`、`sleep_until`、`available_parallelism`、`now` 等（另有 4 项）。
- 上下文/对象类型通常从 `Instant` 等结构开始。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。

### 5.2 单元测试重点
- 建议覆盖公开 API、状态转换和异常分支。

### 5.3 集成测试重点
- 建议补充最小消费者路径，验证该 crate 在真实调用链中可用。

### 5.4 覆盖率要求
- 覆盖率建议：公开 API、边界条件和关键错误处理路径需要显式覆盖。

## 6. 跨项目定位分析
### 6.1 ArceOS
`ax-std` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 用户库层。

### 6.2 StarryOS
当前未检测到 StarryOS 工程本体对 `ax-std` 的显式本地依赖，若参与该系统，通常经外部工具链、配置或更底层生态间接体现。

### 6.3 Axvisor
`ax-std` 不在 Axvisor 目录内部，但被 `axvisor` 等 Axvisor crate 直接依赖，说明它是该系统的共享构件或底层服务。
