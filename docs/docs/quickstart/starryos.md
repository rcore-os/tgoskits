---
sidebar_position: 2
sidebar_label: "StarryOS"
title: "StarryOS 快速上手"
---

# StarryOS 快速上手

StarryOS 的最短启动路径通常包含 rootfs。当前 `qemu` 路径会在缺少 rootfs 时自动补齐，也可以显式先执行 `rootfs`。

## 1. 快速启动

StarryOS 的快速启动比 ArceOS 多了一层 rootfs 资产准备，因此这里同时给出“一步运行”和“显式分步”两种方式。第一次上手时，任选其一即可。

### 1.1 RISC-V 64

`riscv64` 仍然是最适合作为首条验证路径的架构。它在文档和测试套件中都较常用，适合先确认 rootfs 和 QEMU 路径是否已经接通。

推荐第一次从 `riscv64` 开始：

```bash
cargo xtask starry qemu --target riscv64gc-unknown-none-elf
```

或显式分步执行：

```bash
cargo xtask starry rootfs --arch riscv64
cargo xtask starry qemu --target riscv64gc-unknown-none-elf
```

### 1.2 AArch64

如果后续会继续关注板级路径或与 Axvisor 的 AArch64 环境对齐，可以尽快补跑这一条。它也是 StarryOS 当前非常重要的一条验证路径。

```bash
cargo xtask starry qemu --target aarch64-unknown-none-softfloat
```

分步执行：

```bash
cargo xtask starry rootfs --arch aarch64
cargo xtask starry qemu --target aarch64-unknown-none-softfloat
```

### 1.3 x86_64

`x86_64` 适合作为 PC 类平台的补充验证路径。命令和其它架构基本一致，差异主要体现在目标 triple 和对应的 QEMU 配置上。

```bash
cargo xtask starry qemu --target x86_64-unknown-none
```

分步执行：

```bash
cargo xtask starry rootfs --arch x86_64
cargo xtask starry qemu --target x86_64-unknown-none
```

### 1.4 LoongArch64

LoongArch64 路径更适合在主流架构已经跑通之后再验证。这样出现问题时，也更容易区分是环境问题还是实验性架构路径带来的差异。

```bash
cargo xtask starry qemu --target loongarch64-unknown-none-softfloat
```

分步执行：

```bash
cargo xtask starry rootfs --arch loongarch64
cargo xtask starry qemu --target loongarch64-unknown-none-softfloat
```

> `starry rootfs` 当前使用 `--arch`，不是 `--target`。  
> `starry qemu` 的 `--target` 可接受完整 target triple，也可接受简写架构名。

## 2. 测试入口

StarryOS 除了单次启动外，更常见的验证方式是直接进入测试套件。这里的命令会读取 `test-suit/starryos` 下的用例配置，并按分组运行。

StarryOS 的 QEMU 测试分为 `normal` 和 `stress` 两组：

```bash
# 全部 normal 测试
cargo xtask starry test qemu --target riscv64gc-unknown-none-elf

# 压力测试
cargo xtask starry test qemu --target riscv64gc-unknown-none-elf --stress

# 仅运行指定用例
cargo xtask starry test qemu --target aarch64-unknown-none-softfloat -c smoke

# 其他架构
cargo xtask starry test qemu --target x86_64-unknown-none
cargo xtask starry test qemu --target loongarch64-unknown-none-softfloat
```

如果需要板测：

```bash
cargo xtask starry test board -t smoke-orangepi-5-plus --server <ip> --port <port>
```

详细说明见：[StarryOS 测试套件设计](../design/test/starryos)

## 3. 架构选择

如果只是想快速建立对 StarryOS 的整体印象，建议先从推荐路径开始；如果已经明确知道目标平台，也可以直接按下表选择更接近实际场景的架构。

| 架构 | 说明 |
|------|------|
| `riscv64` | 推荐首选，验证链路短 |
| `aarch64` | 默认主路径，适合与 Axvisor 对齐 |
| `x86_64` | 适合 x86 平台适配 |
| `loongarch64` | 实验性支持 |

快速上手页面主要覆盖启动与测试入口。更细的 case 结构、rootfs 组织方式和测试实现细节，可以继续阅读：

- [StarryOS 开发指南](../design/systems/starryos-guide)
- [StarryOS 测试套件设计](../design/test/starryos)
- [QEMU 部署](../manual/deploy/qemu)
