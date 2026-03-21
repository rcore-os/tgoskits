# `axfs` 技术文档

> 路径：`os/arceos/modules/axfs`
> 类型：库 crate
> 分层：ArceOS 层 / ArceOS 内核模块
> 版本：`0.3.0-preview.3`
> 文档依据：当前仓库源码、`Cargo.toml` 与 未检测到 crate 层 README

`axfs` 的核心定位是：ArceOS filesystem module

## 1. 架构设计分析
- 目录角色：ArceOS 内核模块
- crate 形态：库 crate
- 工作区位置：子工作区 `os/arceos`
- feature 视角：主要通过 `times`、`use-ramdisk` 控制编译期能力装配。
- 关键数据结构：可直接观察到的关键数据结构/对象包括 `RootSpec`、`Disk`、`Partition`、`PartitionInfo`、`GptHeader`、`GptPartitionEntry`、`FilesystemType`、`FileType`、`DirEntry`、`FileAttr` 等（另有 5 个关键类型/对象）。

### 1.1 内部模块划分
- `dev`：Block device abstraction for disk operations
- `fs`：Filesystem implementations Ext4 filesystem implementation
- `mounts`：内部子模块
- `partition`：Partition management and filesystem detection This module provides functionality to scan GPT partition tables and detect filesystem types on each partition
- `root`：Root directory of the filesystem TODO: it doesn't work very well if the mount points have containment relationships
- `api`：[std::fs]-like high-level filesystem manipulation operations
- `fops`：Low-level filesystem operations

### 1.2 核心算法/机制
- 该 crate 的实现主要围绕顶层模块分工展开，重点在子系统边界、trait/类型约束以及初始化流程。

## 2. 核心功能说明
- 功能定位：ArceOS filesystem module
- 对外接口：从源码可见的主要公开入口包括 `init_filesystems`、`new`、`size`、`position`、`set_position`、`read_one`、`write_one`、`scan_gpt_partitions`、`RootSpec`、`Disk` 等（另有 5 个公开入口）。
- 典型使用场景：主要服务于 ArceOS 内核模块装配，是运行时、驱动、内存、网络或同步等子系统的一部分。
- 关键调用链示例：按当前源码布局，常见入口/初始化链可概括为 `init_filesystems()` -> `initialize_with_partitions()` -> `parse_root_spec()` -> `parse_device_path()` -> `parse_mmcblk_path()` -> ...。

## 3. 依赖关系图谱
```mermaid
graph LR
    current["axfs"]
    current --> axdriver["axdriver"]
    current --> axerrno["axerrno"]
    current --> axfs_devfs["axfs_devfs"]
    current --> axfs_ramfs["axfs_ramfs"]
    current --> axfs_vfs["axfs_vfs"]
    current --> axio["axio"]
    current --> cap_access["cap_access"]
    current --> lazyinit["lazyinit"]
    arceos_api["arceos_api"] --> current
    arceos_posix_api["arceos_posix_api"] --> current
    axfeat["axfeat"] --> current
    axruntime["axruntime"] --> current
```

### 3.1 直接与间接依赖
- `axdriver`
- `axerrno`
- `axfs_devfs`
- `axfs_ramfs`
- `axfs_vfs`
- `axio`
- `cap_access`
- `lazyinit`
- `rsext4`

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
- `axdma`
- `axdriver_base`
- `axdriver_block`
- 另外还有 `30` 个同类项未在此展开

### 3.3 被依赖情况
- `arceos_api`
- `arceos_posix_api`
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
- 另外还有 `7` 个同类项未在此展开

### 3.5 关键外部依赖
- `axfatfs`
- `log`
- `spin`

## 4. 开发指南
### 4.1 依赖配置
```toml
[dependencies]
axfs = { workspace = true }

# 如果在仓库外独立验证，也可以显式绑定本地路径：
# axfs = { path = "os/arceos/modules/axfs" }
```

### 4.2 初始化流程
1. 在 `Cargo.toml` 中接入该 crate，并根据需要开启相关 feature。
2. 若 crate 暴露初始化入口，优先调用 `init`/`new`/`build`/`start` 类函数建立上下文。
3. 在最小消费者路径上验证公开 API、错误分支与资源回收行为。

### 4.3 关键 API 使用提示
- 优先关注函数入口：`init_filesystems`、`new`、`size`、`position`、`set_position`、`read_one`、`write_one`、`scan_gpt_partitions` 等（另有 32 项）。
- 上下文/对象类型通常从 `RootSpec`、`Disk`、`Partition`、`PartitionInfo`、`GptHeader`、`GptPartitionEntry` 等（另有 5 项） 等结构开始。

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
`axfs` 直接位于 `os/arceos/` 目录树中，是 ArceOS 工程本体的一部分，承担 ArceOS 内核模块。

### 6.2 StarryOS
`axfs` 主要通过 `starry-kernel`、`starryos`、`starryos-test` 等上层 crate 被 StarryOS 间接复用，通常处于更底层的公共依赖层。

### 6.3 Axvisor
`axfs` 主要通过 `axvisor` 等上层 crate 被 Axvisor 间接复用，通常处于更底层的公共依赖层。
