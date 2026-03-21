# `riscv_plic` 技术文档

> 路径：`components/riscv_plic`
> 类型：库 crate
> 分层：组件层 / 可复用基础组件
> 版本：`0.2.0`
> 文档依据：当前仓库源码、`Cargo.toml` 与 `components/riscv_plic/README.md`

`riscv_plic` 的核心定位是：RISC-V platform-level interrupt controller (PLIC) register definitions and basic operations

## 1. 架构设计分析
- 目录角色：可复用基础组件
- crate 形态：库 crate
- 工作区位置：根工作区
- feature 视角：该 crate 没有显式声明额外 Cargo feature，功能边界主要由模块本身决定。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `Plic`、`SOURCE_NUM`、`CONTEXT_NUM`、`U32_BITS`。
- 设计重心：该 crate 多数是寄存器级或设备级薄封装，复杂度集中在 MMIO 语义、安全假设和被上层平台/驱动整合的方式。

### 1.1 内部模块划分
- 当前 crate 未显式声明多个顶层 `mod`，复杂度更可能集中在单文件入口、宏展开或下层子 crate。

### 1.2 核心算法/机制
- 平台中断控制器路由与优先级管理

## 2. 核心功能说明
- 功能定位：RISC-V platform-level interrupt controller (PLIC) register definitions and basic operations
- 对外接口：从源码可见的主要公开入口包括 `init_by_context`、`set_priority`、`get_priority`、`probe_priority_bits`、`is_pending`、`enable`、`disable`、`is_enabled`、`Plic`。
- 典型使用场景：提供寄存器定义、MMIO 访问或设备级操作原语，通常被平台 crate、驱动聚合层或更高层子系统进一步封装。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `init_by_context()` -> `parse_group_and_field()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["riscv_plic"]
    axplat_riscv64_qemu_virt["axplat-riscv64-qemu-virt"] --> current
```

### 3.1 直接与间接依赖
- 未检测到本仓库内的直接本地依赖；该 crate 可能主要依赖外部生态或承担叶子节点角色。

### 3.2 间接本地依赖
- 未检测到额外的间接本地依赖，或依赖深度主要停留在第一层。

### 3.3 被依赖情况
- `axplat-riscv64-qemu-virt`

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
- 另外还有 `27` 个同类项未在此展开

### 3.5 关键外部依赖
- `tock-registers`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
riscv_plic = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# riscv_plic = { path = "components/riscv_plic" }
```

### 4.2 初始化流程
1. 先明确该设备/寄存器组件的调用上下文，是被平台 crate 直接使用还是被驱动聚合层再次封装。
2. 修改寄存器位域、初始化顺序或中断相关逻辑时，应同步检查 `unsafe` 访问、访问宽度和副作用语义。
3. 尽量通过最小平台集成路径验证真实设备行为，而不要只依赖静态接口检查。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`init_by_context`、`set_priority`、`get_priority`、`probe_priority_bits`、`is_pending`、`enable`、`disable`、`is_enabled` 等（另有 5 项）。
- 上下文/对象类型通常从 `Plic` 等结构开始。

## 5. 测试策略
### 5.1 当前仓库内的测试形态
- 当前 crate 目录中未发现显式 `tests/`/`benches/`/`fuzz/` 入口，更可能依赖上层系统集成测试或跨 crate 回归。

### 5.2 单元测试重点
- 建议覆盖寄存器位域、设备状态转换、边界参数和 `unsafe` 访问前提。

### 5.3 集成测试重点
- 建议结合最小平台或驱动集成路径验证真实设备行为，重点检查初始化、中断和收发等主线。

### 5.4 覆盖率要求
- 覆盖率建议：寄存器访问辅助函数和关键状态机保持高覆盖；真实硬件语义以集成验证补齐。

## 6. 跨项目定位分析
### 6.1 ArceOS
`riscv_plic` 主要通过 `arceos-affinity`、`arceos-helloworld`、`arceos-helloworld-myplat`、`arceos-httpclient`、`arceos-httpserver`、`arceos-irq` 等（另有 26 项） 等上层 crate 被 ArceOS 间接复用，通常处于更底层的公共依赖层。

### 6.2 StarryOS
`riscv_plic` 主要通过 `starry-kernel`、`starryos`、`starryos-test` 等上层 crate 被 StarryOS 间接复用，通常处于更底层的公共依赖层。

### 6.3 Axvisor
`riscv_plic` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
