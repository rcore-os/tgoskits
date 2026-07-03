# `ax-api`

> 路径：`os/arceos/api/arceos_api`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 公共 API/feature 聚合层
> 版本：`0.5.0`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`ax-api` 的核心定位是：Public APIs and types for ArceOS modules

## 架构设计
- 目录角色：ArceOS 公共 API/feature 聚合层
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：主要通过 `alloc`、`display`、`dma`、`dummy-if-not-enabled`、`fs`、`ipi`、`irq`、`multitask`、`net`、`paging` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `AxTimeValue`、`DMAInfo`、`AxTaskHandle`、`AxWaitQueueHandle`。

### 模块结构
- `macros`：内部子模块
- `imp`：内部实现细节与 trait/backend 绑定

### 核心机制
- 该 crate 的实现主要围绕顶层模块分工展开，重点在子系统边界、trait/类型约束以及初始化流程。

## 核心功能
- 功能定位：Public APIs and types for ArceOS modules
- 对外接口：从源码可见的主要公开入口包括 `ax_get_cpu_num`、`ax_terminate`、`ax_monotonic_time`、`ax_wall_time`、`ax_alloc`、`ax_dealloc`、`ax_alloc_coherent`、`ax_dealloc_coherent`。
- 典型使用场景：主要作为仓库中的专用支撑 crate 被上层组件调用。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `ax_alloc()` -> `ax_alloc_coherent()` -> `ax_spawn()` -> `ax_open_file()` -> `ax_open_dir()` -> ...。

## 依赖关系
```mermaid
graph LR
    current["ax-api"]
    current --> ax-alloc["ax-alloc"]
    current --> ax-display["ax-display"]
    current --> ax_dma["ax-dma"]
    current --> ax-driver["ax-driver"]
    current --> ax_errno["ax-errno"]
    current --> ax-feat["ax-feat"]
    current --> ax-fs["ax-fs"]
    ax_std["ax-std"] --> current
```

### 直接依赖
- `ax-alloc`
- `ax-display`
- `ax-dma`
- `ax-driver`
- `ax-errno`
- `ax-feat`
- `ax-fs`
- `ax-hal`
- `axio`
- `ax-ipi`
- `ax-log`
- 另外还有 `5` 个同类项未在此展开

### 间接依赖
- `ax-arm-pl031`
- `axaddrspace`
- `ax-allocator`
- `axbacktrace`
- `ax-cpu`
- `rdrive`
- `rdif-block`
- `rdif-display`
- `rdif-input`
- 另外还有 `48` 个同类项未在此展开

### 3.3 被依赖情况
- `ax-std`

### 被依赖情况
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

### 外部依赖
- 当前依赖集合几乎完全来自仓库内本地 crate。

## 开发指南
### 接入方式
```toml
[dependencies]
ax-api = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# ax-api = { path = "os/arceos/api/arceos_api" }
```

### 初始化
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### API 使用
- 优先关注函数入口：`ax_get_cpu_num`、`ax_terminate`、`ax_monotonic_time`、`ax_wall_time`、`ax_alloc`、`ax_dealloc`、`ax_alloc_coherent`、`ax_dealloc_coherent` 等（另有 60 项）。

## 测试
### 测试覆盖
- 当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。

### 单元测试
- 建议覆盖公开 API、状态转换和异常分支。

### 集成测试
- 建议补充最小消费者路径，验证该 crate 在真实调用链中可用。

### 覆盖率
- 覆盖率建议：公开 API、边界条件和关键错误处理路径需要显式覆盖。

## 跨项目定位
### ArceOS
`ax-api` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 公共 API/feature 聚合层。

### StarryOS
当前未检测到 StarryOS 工程本体对 `ax-api` 的显式本地依赖，若参与该系统，通常经外部工具链、配置或更底层生态间接体现。

### Axvisor
`ax-api` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
