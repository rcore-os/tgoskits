# Axvisor 开发指南

Axvisor 在 TGOSKits 里是一条和 ArceOS / StarryOS 并列的系统路径，但它的开发体验和前两者最大的不同是：除了代码，还必须把板级配置、VM 配置和 Guest 镜像一起看。

## 1. Axvisor 在仓库里的位置

| 路径 | 角色 | 什么时候会改到 |
| --- | --- | --- |
| `os/axvisor/src/` | Hypervisor 运行时 | VM 生命周期、调度、设备管理、异常处理 |
| `os/axvisor/configs/board/` | 板级配置 | 目标架构、target、feature、默认 VM 列表 |
| `os/axvisor/configs/vms/` | Guest VM 配置 | kernel 路径、入口地址、内存布局、设备直通 |
| `components/axvm`、`components/axvcpu`、`components/axdevice`、`components/axaddrspace` | 虚拟化核心组件 | VM、vCPU、虚拟设备、地址空间 |
| `components/axvisor_api` | Hypervisor 对外接口 | Guest / Hypervisor 交互接口 |
| `platform/x86-qemu-q35` | x86_64 QEMU Q35 平台实现 | x86_64 板级能力 |

此外，Axvisor 运行时依然大量复用了 ArceOS 能力，例如 `axstd` 和底层平台支持。

## 2. 先记住命令入口

Axvisor 的 build/qemu 不是根 `cargo xtask` 的子命令，而是 `os/axvisor` 自带 xtask 提供的。

你有两种等价写法：

```bash
# 仓库根目录
cargo axvisor defconfig qemu-aarch64
cargo axvisor build
cargo axvisor qemu
```

```bash
# Axvisor 子目录
cd os/axvisor
cargo xtask defconfig qemu-aarch64
cargo xtask build
cargo xtask qemu
```

当前本地 xtask 里最常用的子命令包括：

- `defconfig`
- `build`
- `qemu`
- `menuconfig`
- `image`
- `vmconfig`

## 3. 第一条成功路径：QEMU AArch64

第一次上手建议从 `qemu-aarch64` 开始，因为当前仓库里的现成配置与 CI 入口都优先覆盖这条路径。

### 3.1 推荐方式：使用官方 `setup_qemu.sh`

不要直接从 `defconfig/build/qemu` 开始。  
当前默认 QEMU 模板会引用 `tmp/rootfs.img`，而这个文件不会由 `defconfig` 或 `build` 自动生成。

官方推荐流程是：

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
```

这个脚本会自动完成：

1. 下载并解压 Guest 镜像到 `/tmp/.axvisor-images/qemu_aarch64_arceos`
2. 从 `configs/vms/arceos-aarch64-qemu-smp1.toml` 生成临时 VM 配置  
   输出到 `tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml`
3. 自动修正 VM 配置中的 `kernel_path`
4. 复制 `rootfs.img` 到 `os/axvisor/tmp/rootfs.img`

### 3.2 正确的启动命令

准备完成后，直接运行：

```bash
cd os/axvisor
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

如果一切正常，ArceOS Guest 会输出 `Hello, world!`。

### 3.3 为什么直接 `cargo axvisor qemu` 会失败

原因通常有两个：

1. `configs/board/qemu-aarch64.toml` 当前默认是 `vm_configs = []`
2. 默认 QEMU 配置模板 `scripts/ostool/qemu-aarch64.toml` 会引用 `tmp/rootfs.img`

所以仅执行：

```bash
cargo axvisor defconfig qemu-aarch64
cargo axvisor build
cargo axvisor qemu
```

还不够。除非你已经手工准备好了：

- `.build.toml`
- 可用的 `vmconfigs`
- `os/axvisor/tmp/rootfs.img`

否则 `qemu` 会因为找不到 rootfs 或 VM 配置而失败。

## 4. 组件、运行时和配置是怎样连起来的

```mermaid
flowchart TD
    HvCrates["components/axvm + axvcpu + axdevice + axaddrspace"]
    ArceosBase["ArceOS base capabilities"]
    Runtime["os/axvisor/src/*"]
    BoardCfg["configs/board/*.toml -> .build.toml"]
    VmCfg["configs/vms/*.toml"]
    GuestImg["Guest image"]
    QemuRun["cargo axvisor qemu"]

    HvCrates --> Runtime
    ArceosBase --> Runtime
    Runtime --> QemuRun
    BoardCfg --> QemuRun
    VmCfg --> QemuRun
    GuestImg --> VmCfg
```

这条链路说明了四类不同改动：

- 代码实现：`components/*` 或 `os/axvisor/src/*`
- 板级能力：`configs/board/*`、`platform/x86-qemu-q35`
- 单个 Guest 启动参数：`configs/vms/*`
- Guest 本身内容：外部生成的 Guest 镜像

## 5. 常见开发动作

### 5.1 修改虚拟化核心组件

如果你改的是：

- `components/axvm`
- `components/axvcpu`
- `components/axdevice`
- `components/axaddrspace`

通常先做 build-only 验证：

```bash
cargo axvisor defconfig qemu-aarch64
cargo axvisor build
```

只有在 Guest 镜像和 VM 配置都已准备好的前提下，再跑：

```bash
cargo axvisor qemu
```

### 5.2 修改 Hypervisor 运行时

`os/axvisor/src/*` 更偏系统整合层。  
这类改动常常会同时依赖：

- 板级 feature 是否启用正确
- Guest 的 `kernel_path` 是否正确
- 设备直通或内存区域配置是否一致

所以排查顺序通常是：

1. 先确认 `build` 成功
2. 再确认 `.build.toml` 和 `configs/vms/*.toml`
3. 最后再判断是不是运行时代码本身的问题

### 5.3 新增板级支持

新增板级支持往往需要两部分一起落地：

- `os/axvisor/configs/board/<board>.toml`
- 对应的平台 crate，例如 `components/axplat_crates/platforms/*` 或 `platform/x86-qemu-q35`

当前仓库里现成的板级配置包括：

- `qemu-aarch64.toml`
- `qemu-x86_64.toml`
- `orangepi-5-plus.toml`
- `phytiumpi.toml`
- `roc-rk3568-pc.toml`

### 5.4 调整 VM 配置

如果你只是想切换 Guest 或修改 Guest 资源分配，最常见的入口就是 `configs/vms/*.toml`：

- `kernel_path`
- `entry_point`
- `cpu_num`
- `memory_regions`
- `passthrough_devices`
- `excluded_devices`

这类改动通常不需要动 Hypervisor 主代码，但经常会决定你能不能真正把 Guest 拉起来。

## 6. 最常用的验证入口

### build-only 验证

```bash
cd os/axvisor
cargo xtask defconfig qemu-aarch64
cargo xtask build
```

### 运行 QEMU 验证

```bash
cd os/axvisor
./scripts/setup_qemu.sh arceos
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

### 根工作区测试入口

```bash
cargo xtask test axvisor --target aarch64-unknown-none-softfloat
```

这条命令属于根工作区测试矩阵，不等价于本地 `cargo xtask qemu ...`。  
它会走自己的测试逻辑，并自动确保所需镜像已下载；当前 AArch64 测试默认使用的是 Linux guest 测试配置，而不是你手工运行的 ArceOS guest 路径。

### x86_64 路径

如果你在做 x86_64 相关改动，可以切到：

```bash
cargo axvisor defconfig qemu-x86_64
cargo axvisor build
```

这时常常还要一起关注 `platform/x86-qemu-q35`。

## 7. 调试建议

### 先看配置，再看代码

Axvisor 启动失败时，最常见的问题不是 Rust 代码编译失败，而是下面四件事没对齐：

1. `.build.toml` 是不是当前想要的板级配置
2. `vm_configs` 是不是空的
3. `configs/vms/*.toml` 里的 `kernel_path` 是否真实存在
4. Guest 镜像的入口地址、加载地址、内存布局是否匹配

### 调整日志和配置

最直接的做法是：

- 先在板级配置里调高 `log`
- 再重新执行 `defconfig`
- 需要交互式调整时用 `cargo axvisor menuconfig`

### 哪些命令适合排错

```bash
# 重新生成当前配置
cargo axvisor defconfig qemu-aarch64

# 查看或调整配置
cargo axvisor menuconfig

# 只做构建，先排除编译问题
cargo axvisor build

# 使用官方脚本准备镜像和 rootfs
cd os/axvisor
./scripts/setup_qemu.sh arceos

# 明确指定 VM 配置运行
cargo xtask qemu \
  --build-config configs/board/qemu-aarch64.toml \
  --qemu-config .github/workflows/qemu-aarch64.toml \
  --vmconfigs tmp/vmconfigs/arceos-aarch64-qemu-smp1.generated.toml
```

## 8. 继续往哪里读

- [axvisor-internals.md](axvisor-internals.md): 系统理解 Axvisor 的五层架构、VMM 启动链、vCPU 任务模型与 `axvisor_api`
- [components.md](components.md): 从组件角度看 Axvisor 与 ArceOS / StarryOS 的共享依赖
- [build-system.md](build-system.md): 理解 `cargo axvisor` 与根 `cargo xtask test axvisor` 的边界
- [quick-start.md](quick-start.md): 如果你只是想先把第一条 QEMU 路径跑通
