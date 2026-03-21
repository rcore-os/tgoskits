# `starry-vm` 技术文档

> 路径：`components/starry-vm`
> 类型：库 crate
> 分层：组件层 / 可复用基础组件
> 版本：`0.3.0`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`starry-vm` 的核心定位是：Virtual memory management library for Starry OS

## 1. 架构设计分析
- 目录角色：可复用基础组件
- crate 形态：库 crate
- 工作区位置：根工作区
- feature 视角：主要通过 `alloc` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `VmError`、`VmResult`、`Target`、`VmMutPtr`、`VmPtr`、`MAX_BYTES`、`CHUNK_SIZE`。
- 设计重心：该 crate 通常作为多个内核子系统共享的底层构件，重点在接口边界、数据结构和被上层复用的方式。

### 1.1 内部模块划分
- `thin`：内部子模块
- `alloc`：内部子模块（按 feature: alloc 条件启用）

### 1.2 核心算法/机制
- 内存分配器初始化、扩容或对象分配路径
- 虚拟机生命周期、资源模型与状态切换

## 2. 核心功能说明
- 功能定位：Virtual memory management library for Starry OS
- 对外接口：从源码可见的主要公开入口包括 `vm_read_slice`、`vm_write_slice`、`vm_load_any`、`vm_load`、`vm_load_until_nul`、`VmError`、`VmPtr`、`VmMutPtr`。
- 典型使用场景：作为共享基础设施被多个 OS 子系统复用，常见场景包括同步、内存管理、设备抽象、接口桥接和虚拟化基础能力。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `new()` -> `vm_load_any()` -> `vm_load()` -> `vm_load_until_nul()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["starry-vm"]
    current --> axerrno["axerrno"]
    starry_kernel["starry-kernel"] --> current
    starry_signal["starry-signal"] --> current
```

### 3.1 直接与间接依赖
- `axerrno`

### 3.2 间接本地依赖
- 未检测到额外的间接本地依赖，或依赖深度主要停留在第一层。

### 3.3 被依赖情况
- `starry-kernel`
- `starry-signal`

### 3.4 间接被依赖情况
- `starryos`
- `starryos-test`

### 3.5 关键外部依赖
- `bytemuck`
- `extern-trait`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
starry-vm = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# starry-vm = { path = "components/starry-vm" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`vm_read_slice`、`vm_write_slice`、`vm_load_any`、`vm_load`、`vm_load_until_nul`。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 存在 crate 内集成测试：`tests/test.rs`。

### 5.2 单元测试重点
- 建议用单元测试覆盖公开 API、错误分支、边界条件以及并发/内存安全相关不变量。

### 5.3 集成测试重点
- 建议补充被 ArceOS/StarryOS/Axvisor 消费时的最小集成路径，确保接口语义与 feature 组合稳定。

### 5.4 覆盖率要求
- 覆盖率建议：核心算法与错误路径达到高覆盖，关键数据结构和边界条件应实现接近完整覆盖。

## 6. 跨项目定位分析
### 6.1 ArceOS
当前未检测到 ArceOS 工程本体对 `starry-vm` 的显式本地依赖，若参与该系统，通常经外部工具链、配置或更底层生态间接体现。

### 6.2 StarryOS
`starry-vm` 不在 StarryOS 目录内部，但被 `starry-kernel` 等 StarryOS crate 直接依赖，说明它是该系统的共享构件或底层服务。

### 6.3 Axvisor
从命名与目录角色判断，`starry-vm` 与虚拟化栈语义高度相关，但当前仓库中未检测到更多 Axvisor Rust crate 直接消费它。
