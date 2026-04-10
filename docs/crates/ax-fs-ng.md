# `ax-fs-ng` 技术文档

> 路径：`os/arceos/modules/axfs-ng`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 内核模块
> 版本：`0.5.0`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`ax-fs-ng` 的核心定位是：ArceOS filesystem module

## 1. 架构设计分析
- 目录角色：ArceOS 内核模块
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：主要通过 `ext4`、`fat`、`std`、`times` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `DefaultFilesystem`、`Initialize`。

### 1.1 内部模块划分
- `fs`：文件系统、挂载或路径解析逻辑
- `highlevel`：内部子模块

### 1.2 核心算法/机制
- 该 crate 的实现主要围绕顶层模块分工展开，重点在子系统边界、trait/类型约束以及初始化流程。

## 2. 核心功能说明
- 功能定位：ArceOS filesystem module
- 对外接口：从源码可见的主要公开入口包括 `init_filesystems`、`new`、`new_default`、`DefaultFilesystem`。
- 典型使用场景：主要作为仓库中的专用支撑 crate 被上层组件调用。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `init_filesystems()` -> `new()` -> `new_default()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["ax-fs-ng"]
    current --> ax-alloc["ax-alloc"]
    current --> ax-driver["ax-driver"]
    current --> ax_errno["ax-errno"]
    current --> axfs_ng_vfs["axfs-ng-vfs"]
    current --> ax-hal["ax-hal"]
    current --> axio["ax-io"]
    current --> axpoll["axpoll"]
    current --> ax-sync["ax-sync"]
    ax_feat["ax-feat"] --> current
    ax_net_ng["ax-net-ng"] --> current
    ax_runtime["ax-runtime"] --> current
    starry_kernel["starry-kernel"] --> current
```

### 3.1 直接与间接依赖
- `ax-alloc`
- `ax-driver`
- `ax-errno`
- `axfs-ng-vfs`
- `ax-hal`
- `axio`
- `axpoll`
- `ax-sync`
- `ax-kspin`
- `scope-local`

### 3.2 间接本地依赖
- `ax-arm-pl011`
- `ax-arm-pl031`
- `axaddrspace`
- `ax-allocator`
- `axbacktrace`
- `axconfig`
- `ax-config-gen`
- `ax-config-macros`
- `ax-cpu`
- `ax-dma`
- `ax-driver-base`
- `axdriver_block`
- 另外还有 `37` 个同类项未在此展开

### 3.3 被依赖情况
- `ax-feat`
- `ax-net-ng`
- `ax-runtime`
- `starry-kernel`

### 3.4 间接被依赖情况
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
- 另外还有 `16` 个同类项未在此展开

### 3.5 关键外部依赖
- `bitflags`
- `cfg-if`
- `chrono`
- `intrusive-collections`
- `log`
- `lru`
- `lwext4_rust`
- `slab`
- `spin`
- `starry-fatfs`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
ax-fs-ng = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# ax-fs-ng = { path = "os/arceos/modules/axfs-ng" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`init_filesystems`、`new`、`new_default`。
- 上下文/对象类型通常从 `DefaultFilesystem` 等结构开始。

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
`ax-fs-ng` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 内核模块。

### 6.2 StarryOS
`ax-fs-ng` 不在 StarryOS 目录内部，但被 `starry-kernel` 等 StarryOS crate 直接依赖，说明它是该系统的共享构件或底层服务。

### 6.3 Axvisor
`ax-fs-ng` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
