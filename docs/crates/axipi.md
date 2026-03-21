# `axipi` 技术文档

> 路径：`os/arceos/modules/axipi`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 内核模块
> 版本：`0.3.0-preview.3`
> 文档依据：当前仓库源码、`Cargo.toml` 与 `os/arceos/modules/axipi/README.md`

`axipi` 的核心定位是：ArceOS IPI management module

## 1. 架构设计分析
- 目录角色：ArceOS 内核模块
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：该 crate 没有显式声明额外 Cargo feature，功能边界主要由模块本身决定。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `Callback`、`MulticastCallback`、`IpiEvent`、`IpiEventQueue`、`IPI_EVENT_QUEUE`。

### 1.1 内部模块划分
- `event`：内部子模块
- `queue`：内部子模块

### 1.2 核心算法/机制
- 跨核 IPI 协调与唤醒路径
- 队列管理、调度或异步事件缓存

## 2. 核心功能说明
- 功能定位：ArceOS IPI management module
- 对外接口：从源码可见的主要公开入口包括 `init`、`run_on_cpu`、`run_on_each_cpu`、`ipi_handler`、`new`、`call`、`into_unicast`、`is_empty`、`Callback`、`MulticastCallback` 等（另有 2 个公开入口）。
- 典型使用场景：主要服务于 ArceOS 内核模块装配，是运行时、驱动、内存、网络或同步等子系统的一部分。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `init()` -> `run_on_cpu()` -> `run_on_each_cpu()` -> `new()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["axipi"]
    current --> axconfig["axconfig"]
    current --> axhal["axhal"]
    current --> kspin["kspin"]
    current --> lazyinit["lazyinit"]
    current --> percpu["percpu"]
    arceos_api["arceos_api"] --> current
    axfeat["axfeat"] --> current
    axruntime["axruntime"] --> current
```

### 3.1 直接与间接依赖
- `axconfig`
- `axhal`
- `kspin`
- `lazyinit`
- `percpu`

### 3.2 间接本地依赖
- `arm_pl011`
- `arm_pl031`
- `axalloc`
- `axallocator`
- `axbacktrace`
- `axconfig-gen`
- `axconfig-macros`
- `axcpu`
- `axdriver_base`
- `axdriver_block`
- `axdriver_display`
- `axdriver_input`
- 另外还有 `23` 个同类项未在此展开

### 3.3 被依赖情况
- `arceos_api`
- `axfeat`
- `axruntime`

### 3.4 间接被依赖情况
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
- 另外还有 `8` 个同类项未在此展开

### 3.5 关键外部依赖
- `log`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
axipi = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# axipi = { path = "os/arceos/modules/axipi" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`init`、`run_on_cpu`、`run_on_each_cpu`、`ipi_handler`、`new`、`call`、`into_unicast`、`is_empty` 等（另有 2 项）。
- 上下文/对象类型通常从 `Callback`、`MulticastCallback`、`IpiEvent`、`IpiEventQueue` 等结构开始。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。

### 5.2 单元测试重点
- 建议围绕 API 契约、feature 分支、资源管理和错误恢复路径编写单元测试。

### 5.3 集成测试重点
- 建议至少补一条 ArceOS 示例或 `test-suit/arceos` 路径，必要时覆盖多架构或多 feature 组合。

### 5.4 覆盖率要求
- 覆盖率建议：公开 API、初始化失败路径和主要 feature 组合必须覆盖；涉及调度/内存/设备时需补系统级验证。

## 6. 跨项目定位分析
### 6.1 ArceOS
`axipi` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 内核模块。

### 6.2 StarryOS
`axipi` 主要通过 `starry-kernel`、`starryos`、`starryos-test` 等上层 crate 被 StarryOS 间接复用，通常处于更底层的公共依赖层。

### 6.3 Axvisor
`axipi` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
