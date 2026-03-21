# `axdriver_block` 技术文档

> 路径：`components/axdriver_crates/axdriver_block`
> 类型：库 crate
> 分层：组件层 / 可复用基础组件
> 版本：`0.1.4-preview.3`
> 文档依据：当前仓库源码、`Cargo.toml` 与 `components/axdriver_crates/axdriver_block/README.md`

`axdriver_block` 的核心定位是：Common traits and types for block storage drivers

## 1. 架构设计分析
- 目录角色：可复用基础组件
- crate 形态：库 crate
- 工作区位置：子工作区 `components/axdriver_crates`
- feature 视角：主要通过 `ahci`、`bcm2835-sdhci`、`ramdisk`、`ramdisk-static`、`sdmmc` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `SDHCIDriver`、`RamDisk`、`AhciDriver`、`SdMmcDriver`、`Target`、`BaseDriverOps`、`DevError`、`DevResult`、`BLOCK_SIZE`。
- 设计重心：该 crate 多数是寄存器级或设备级薄封装，复杂度集中在 MMIO 语义、安全假设和被上层平台/驱动整合的方式。

### 1.1 内部模块划分
- `bcm2835sdhci`：SD card driver for raspi4（按 feature: bcm2835-sdhci 条件启用）
- `ramdisk`：A RAM disk driver backed by heap memory（按 feature: ramdisk 条件启用）
- `ramdisk_static`：A RAM disk driver backed by a static slice（按 feature: ramdisk-static 条件启用）
- `ahci`：AHCI driver（按 feature: ahci 条件启用）
- `sdmmc`：SD/MMC driver based on SDIO（按 feature: sdmmc 条件启用）

### 1.2 核心算法/机制
- 该 crate 的实现主要围绕顶层模块分工展开，重点在子系统边界、trait/类型约束以及初始化流程。

## 2. 核心功能说明
- 功能定位：Common traits and types for block storage drivers
- 对外接口：从源码可见的主要公开入口包括 `try_new`、`new`、`SDHCIDriver`、`RamDisk`、`AhciDriver`、`SdMmcDriver`、`BlockDriverOps`。
- 典型使用场景：提供寄存器定义、MMIO 访问或设备级操作原语，通常被平台 crate、驱动聚合层或更高层子系统进一步封装。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `try_new()` -> `new()`。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["axdriver_block"]
    current --> axdriver_base["axdriver_base"]
    axdriver["axdriver"] --> current
    axdriver_virtio["axdriver_virtio"] --> current
    axplat_dyn["axplat-dyn"] --> current
```

### 3.1 直接与间接依赖
- `axdriver_base`

### 3.2 间接本地依赖
- 未检测到额外的间接本地依赖，或依赖深度主要停留在第一层。

### 3.3 被依赖情况
- `axdriver`
- `axdriver_virtio`
- `axplat-dyn`

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
- 另外还有 `23` 个同类项未在此展开

### 3.5 关键外部依赖
- `bcm2835-sdhci`
- `log`
- `simple-ahci`
- `simple-sdmmc`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
axdriver_block = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# axdriver_block = { path = "components/axdriver_crates/axdriver_block" }
```

### 4.2 初始化流程
1. 先明确该设备/寄存器组件的调用上下文，是被平台 crate 直接使用还是被驱动聚合层再次封装。
2. 修改寄存器位域、初始化顺序或中断相关逻辑时，应同步检查 `unsafe` 访问、访问宽度和副作用语义。
3. 尽量通过最小平台集成路径验证真实设备行为，而不要只依赖静态接口检查。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`try_new`、`new`。
- 上下文/对象类型通常从 `SDHCIDriver`、`RamDisk`、`AhciDriver`、`SdMmcDriver` 等结构开始。

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
`axdriver_block` 不在 ArceOS 目录内部，但被 `axdriver` 等 ArceOS crate 直接依赖，说明它是该系统的共享构件或底层服务。

### 6.2 StarryOS
`axdriver_block` 主要通过 `starry-kernel`、`starryos`、`starryos-test` 等上层 crate 被 StarryOS 间接复用，通常处于更底层的公共依赖层。

### 6.3 Axvisor
`axdriver_block` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
