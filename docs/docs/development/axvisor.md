# Axvisor 开发指南

Axvisor 的开发体验与 ArceOS / StarryOS 最大的不同是：除了代码，还必须把板级配置、VM 配置和 Guest 镜像一起看。本文聚焦**开发闭环——改了什么配置/代码之后该如何验证**，以及调试技巧。

> 架构分层、运行时模块和核心设计机制见 [Axvisor 架构](/docs/architecture/axvisor)。  
> 最短命令和快速启动见 [Axvisor 快速上手](/docs/quickstart/axvisor)。  
> 构建系统总览见 [build-system.md](/docs/build/overview)。

## 第一条成功路径：QEMU AArch64

第一次上手建议从 `qemu-aarch64` 开始。

### 使用官方 `setup_qemu.sh`

不要直接从 `defconfig/build/qemu` 开始——默认 QEMU 模板引用的 `tmp/rootfs.img` 不会由 `defconfig` 或 `build` 自动生成。

```bash
cargo xtask axvisor defconfig qemu-aarch64
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)
```

这个脚本会自动完成：

1. 下载并解压 Guest 镜像到 `/tmp/.axvisor-images/qemu_aarch64_arceos`
2. 从 `configs/vms/arceos-aarch64-qemu-smp1.toml` 生成 `os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
3. 自动修正 VM 配置中的 `kernel_path`
4. 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

### 正确的启动命令

```bash
cargo xtask axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

### 为什么直接跑会失败

原因通常有两个：

1. 默认板级配置里的 `vm_configs` 可能是空的
2. 默认 QEMU 配置模板会引用 `os/axvisor/tmp/rootfs.img`

仅执行 `defconfig → build → qemu` 通常不够，还需要准备 `.build.toml`、可用的 `vmconfigs` 和 `rootfs.img`。

## 常见开发动作

### 修改虚拟化核心组件

如果你改的是 `components/axvm`、`axvcpu`、`axdevice`、`axaddrspace`：

先做 build-only 验证：

```bash
cargo xtask axvisor defconfig qemu-aarch64
cargo xtask axvisor build --config os/axvisor/.build.toml
```

Guest 镜像和 VM 配置都准备好后，再跑 QEMU。

### 修改 Hypervisor 运行时

`os/axvisor/src/*` 偏系统整合层，常常同时依赖：

- 板级 feature 是否启用正确
- Guest 的 `kernel_path` 是否正确
- 设备直通或内存区域配置是否一致

### 新增板级支持

需要两部分一起落地：

- `os/axvisor/configs/board/<board>.toml`
- 对应的平台 crate

当前板级配置：`qemu-aarch64`、`qemu-loongarch64`、`qemu-riscv64`、`qemu-x86_64`、`orangepi-5-plus`、`phytiumpi`、`rdk-s100`、`roc-rk3568-pc`、`tac-e400`。

### 调整 VM 配置

切换 Guest 或修改资源分配的入口是 `configs/vms/*.toml`。支持的 Guest 类型：ArceOS、Linux、FreeRTOS、Zephyr、NimbOS、RT-Thread。

VM 配置关键字段：`kernel_path`、`entry_point`、`cpu_num`、`memory_regions`、`passthrough_devices`、`excluded_devices`。

## 调试建议

### 先看配置，再看代码

Axvisor 启动失败时，最常见的问题不是 Rust 代码编译失败，而是下面四件事没对齐：

1. `.build.toml` 是不是当前想要的板级配置
2. `vm_configs` 是不是空的
3. `configs/vms/*.toml` 里的 `kernel_path` 是否真实存在
4. Guest 镜像的入口地址、加载地址、内存布局是否匹配

### 排错命令

```bash
# 重新生成当前配置
cargo xtask axvisor defconfig qemu-aarch64

# 查看可用板级配置
cargo xtask axvisor config ls

# 只做构建，先排除编译问题
cargo xtask axvisor build --config os/axvisor/.build.toml

# 使用官方脚本准备镜像和 rootfs
(cd os/axvisor && ./scripts/setup_qemu.sh arceos)

# 明确指定 VM 配置运行
cargo xtask axvisor qemu \
  --config os/axvisor/.build.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs os/axvisor/tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

## 继续往哪里读

- [Axvisor 架构](/docs/architecture/axvisor): 五层架构、VMM 启动链、vCPU 任务模型
- [components.md](/docs/development/components): Axvisor 与 ArceOS / StarryOS 的共享依赖
- [build-system.md](/docs/build/overview): xtask、辅助脚本与测试入口边界
- [arceos-guide.md](/docs/development/arceos): Axvisor 所依赖的 ArceOS 基础能力
