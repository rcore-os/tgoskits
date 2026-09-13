# LoongArch 中断控制器 OS 无关化设计

## 背景与目标

当前 LoongArch EIOINTC、PCH-PIC 与 LIOINTC 的寄存器实现位于
`somehal`。硬件寄存器语义、FDT/ACPI 探测、MMIO 映射、IRQ domain、
`rdrive` 注册和 hard-IRQ dispatch 因此处于同一层，其他内核或
hypervisor 无法复用控制器逻辑，host 侧也难以完整验证初始化与寄存器时序。

本设计新增一个 `#![no_std]` crate：
`drivers/intc/loongarch-intc-driver`。它承担三类控制器的寄存器协议与可选
`rdif-intc` capability，目标调用方是 `somehal` 以及后续需要复用 LoongArch
irqchip 的内核/运行时。

完成标准如下：

- EIOINTC、PCH-PIC、LIOINTC 的硬件实现只存在于新 crate；
- crate 不依赖 `somehal`、`someboot`、`rdrive`、FDT/ACPI parser 或 OS 锁；
- `somehal` 保持现有 firmware wire format、三个独立 IRQ domain 与 trap 分类；
- hard IRQ claim/complete 不获取 controller/rdrive 锁；
- host 测试可观察初始化、路由、mask、claim 和 complete 的真实寄存器操作；
- LoongArch QEMU 与 JL-LSGD2K10 的真实 LIOINTC/AHCI IRQ 链通过验证。

不实现会继续把一份平台 glue 当作唯一硬件实现，增加重复实现、锁误用和无法
在 host 稳定回归寄存器顺序的风险。

## 范围与非目标

本次保持 CPU0、单 EIO 节点与现有 LS2K1000 行为，不改变 compatible、
ACPI/FDT specifier、现有 `IrqId`、domain kind 或 probe priority。

以下内容不属于本次范围：

- timer、IPI、ECFG/ESTAT trap 分类；它们仍由 `someboot`/`somehal` 管理；
- NUMA/多 EIO 或 PCH-PIC 节点、CPU affinity、CPU hotplug；
- LIOINTC ACPI 枚举；
- Linux irqchip 的全部层级 domain、resume 和 syscore 语义；
- Axvisor LVZ smoke、crates.io 发布。

## 现有实现与 prior art

### 仓库内部

`arm-gic-driver` 和 `x86-apic-driver` 已确立以下边界：驱动 crate 提供
`no_std` 寄存器核心，并通过可选 `rdif` feature 实现
`rdif_intc::Interface`；平台 glue 负责 firmware discovery、`ioremap`、
domain 分配、`rdrive` 注册及 CPU trap/EOI 策略。本设计沿用该模式，但把
EIO、PCH 和 LIO 放进同一个 LoongArch irqchip crate，以便显式表达级联关系和
共享强类型。

### Linux v7.1

参考本地 Linux v7.1，commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `drivers/irqchip/irq-loongson-eiointc.c`：EIO vector bitmap、NODEMAP、
  IPMAP、ROUTE、BOUNCE，以及 ISR read/W1 clear；
- `drivers/irqchip/irq-loongson-pch-pic.c`：MASK、EDGE、POL、HTVEC 与父级联
  顺序；
- `drivers/irqchip/irq-loongson-liointc.c`：32 inputs、最多 4 条 parent CPU
  line、per-core ISR、W1 enable/disable 且无独立 EOI；
- `Documentation/devicetree/bindings/interrupt-controller/loongson,*.yaml`：
  firmware binding。

本次借鉴的是硬件寄存器语义、级联结构和 mask/route/ack 顺序，不复制 Linux
的 irqdomain、锁、syscore 或 NUMA 生命周期。TGOSKits 继续使用三个独立动态
domain，并由 `somehal` 附加 OS 侧所有权。

## 方案比较

| 方案 | 优点 | 代价与结论 |
| --- | --- | --- |
| 保持 `somehal` 内实现 | 改动最少 | 无法跨 OS 复用，硬件与 probe/锁继续耦合；不选 |
| 三个独立 crate | 单设备边界最小 | 级联强类型与共同错误重复，workspace/API 成本偏高；不选 |
| 单一 LoongArch irqchip crate | 与 GIC/x86 模式一致，统一强类型和测试，同时保留三个 controller | crate 内有三个领域模块，但变化原因仍是同一架构 irqchip；选择 |
| 把三个 controller 合成一个 RDIF domain | 调用表面较小 | 改变现有 domain 身份和 firmware 语义，掩盖 PCH/EIO 级联；不选 |

## 拓扑与命名空间

```text
CPU interrupt lines
├─ timer / IPI ─────────────── someboot + somehal
├─ LIO cascade ── LioCpuIf ── LIO domain
└─ EIO cascade ── EioCpuIf ── EIO vector
                              └─ PchCpuIf ── PCH domain/input
```

`CpuIrqLine`、`EioVector`、`PchInput`、`LioInput` 是不同 newtype。CPU trap
line、controller-local `HwIrq` 与平台 `IrqId { domain, hwirq }` 不允许通过
裸整数算术混用。FDT/ACPI translation 只返回 controller-local `HwIrq`；
`rdif_intc::Intc::new(domain, controller)` 是唯一附加 domain 的位置。

## Crate 边界与依赖

```text
somehal
  ├─ firmware parsing / ioremap / module_driver / domain / rdrive
  ├─ cascade enable and rollback
  └─ ActiveIrq + Drop completion
          │
          ▼
loongarch-intc-driver
  ├─ eio: IOCSR protocol + controller/cpu interface
  ├─ pch: mapped MMIO protocol + controller/cpu interface
  ├─ lio: mapped MMIO protocol + controller/cpu interface
  └─ rdif (optional)
          │
          ├─ mmio-api
          ├─ tock-registers
          ├─ rdif-intc (optional)
          ├─ thiserror
          └─ loongArch64 (target-only NativeIocsr)
```

crate 不执行 `ioremap`，不解析 FDT/ACPI，不分配 domain，不注册设备，也不调用
其他 controller 或 OS 回调。PCH 的 parent sequencing 因而属于 `somehal`，而
不是 `PchPicController::set_enabled` 的隐藏副作用。PCH/LIO 在保留
`mmio-api::MmioRaw` 映射 capability 的同时，用 `tock-registers` 描述固定布局和
volatile register 类型，避免平台代码重复 offset、指针运算和读改写细节。

中断控制器不拥有 DMA buffer、device-visible address 或 cache-coherency 转换，
因此本 crate 不引入 `dma-api`。为没有 DMA 语义的 irqchip 构造空 DMA capability
只会模糊边界；后续若控制器扩展真实 DMA 数据路径，再按该资源的所有权接入。

## 公共 API

### 强类型与错误

四类 ID 通过 fallible constructor 验证范围，并提供 `index()`/`raw()` 只读
访问。`IntcError` 是可匹配的 `thiserror::Error`，至少区分：

- 空或越界 firmware specifier；
- 无效 vector/input/CPU line；
- 零长度、不足以覆盖必需寄存器或未满足 typed register block 自然对齐的
  MMIO；
- 零 vector count、vector 区间溢出或超过硬件上限；
- LIO parent map 与 parent line 不一致；
- ACPI controller identity、translation 或配置不匹配。

### EIOINTC

`IocsrAccess` 是最小寄存器 capability，提供 32/64-bit IOCSR read/write。
`NativeIocsr` 仅在 LoongArch target 上实现该 capability；host 测试注入 fake
backend。CPU interface 会在 hard IRQ 中调用 64-bit read/write，因此生产
backend 必须 IRQ-safe 且有界：不能睡眠、分配、获取阻塞锁或回调 OS 服务。
构造入口为：

```rust,ignore
let EioIntcParts {
    controller,
    cpu_interface,
} = EioIntcParts::new(iocsr, EioIntcConfig::new(vector_count)?)?;
```

controller 拥有 init/enable/disable；CPU interface 拥有 pending claim 与 W1
complete。两端持有同一个 caller-supplied IOCSR capability 的副本，但不共享
controller lock。当前硬件与平台仍只启用 CPU0/单节点路由。

### PCH-PIC

构造入口为：

```rust,ignore
let PchPicParts {
    controller,
    cpu_interface,
} = PchPicParts::new(mapped_mmio, PchPicConfig::new(base, count)?)?;
```

controller 拥有 MASK、EDGE、POL、HTVEC；不可变 CPU interface 只把 EIO
vector 映射为 PCH input。映射由 `base_vector + input` 唯一决定，不维护第二份
动态 ACPI route cache。ACPI `route.vector` 不是 EIO hardware vector。

FDT 的 `loongson,pic-num-vecs` 是显式硬件 input count；未提供时从 PCH-PIC
ID 寄存器的 bits `[55:48] + 1` 探测。ACPI MADT `BIO_PIC` entry 不携带该
count，`rdrive::AcpiPchPic::gsi_count` 只是 firmware GSI routing span（当前可为
256），不能作为控制器实际 input count。ACPI probe 因而始终以硬件 ID 为准，
避免把路由命名空间大小误当成 MASK/EDGE/POL 寄存器支持的输入数。

PCH `rdif_intc::Interface::set_enabled` 只控制本地 mask/HTVEC。`somehal`
对 enable/disable 都保持“父 EIO 后本地 PCH”的现有顺序；本地步骤失败时把父
EIO 回滚到转换前状态。两个 `rdrive` controller lock 不嵌套持有。

### LIOINTC

构造入口为：

```rust,ignore
let LioIntcParts {
    controller,
    cpu_interface,
} = LioIntcParts::new(regs, isr, config)?;
```

config 包含最多四条 parent CPU line 及每条 line 的 input bitmap。controller
初始化 route bytes、W1 disable、edge/polarity，并拥有 enable/disable 写入。
CPU interface 拥有 ISR mapping、parent lines 和一个预分配的原子 enabled
snapshot。两端仅通过这个 snapshot 共享状态：

- enable：先写 `REG_ENABLE`，再以 Release 发布 input；
- disable：先以 AcqRel 从 snapshot 隐藏 input，再写 `REG_DISABLE`；
- claim：Acquire 读取 snapshot，并与触发 parent 的 effective input bitmap 及 ISR
  相交；未被 firmware bitmap 选中的 fallback input 只属于首个有效 parent；
- complete：验证 domain/input 后不写硬件，因为当前输入为 level，设备 handler
  负责 deassert。

hard IRQ 不访问 task-owned controller 或其锁。

## 初始化、发布与失败回滚

`somehal` 的顺序为：

1. 解析 firmware 并映射所需 MMIO；
2. 构造 `Parts` 并初始化硬件（parent cascade 仍关闭）；
3. 分配 domain；
4. 把 shutdown-lifetime CPU interface 发布给 hard-IRQ 路径；
5. 通过 `rdif_intc::Intc` 注册 controller；
6. 发布 domain/registered 状态；
7. 最后开启 parent cascade line。

EIO 和 LIO 都遵循该顺序，避免 hard IRQ 观察到尚未完成的 domain/controller。
PCH 没有独立 CPU cascade line；它通过已发布的 EIO CPU interface 解析 vector。

PCH 本地步骤失败时，glue 把已经转换的父 EIO vector 回滚到原状态；父级步骤
失败时不会触碰本地 PCH，回滚本身失败则保留两份错误信息。probe 构造/注册失败
保持 cascade 关闭，且不发布半初始化 CPU interface。

## MMIO/IOCSR 安全边界

- `somehal::ioremap` 负责建立和维持 `MmioRaw` 映射生命周期；驱动只在传入
  region 内 volatile 访问。
- constructors 在第一次访问前验证 PCH region 覆盖 `POL`，LIO regs 覆盖
  route/control registers，LIO ISR 至少覆盖一个 `u32`，并验证 PCH/LIO typed
  register block 的自然对齐。
- `MmioRaw::new` 的 pointer validity 与映射生命周期安全责任留在 OS glue；
  driver 在构造寄存器引用前拒绝未对齐地址，且不从物理地址构造指针。
- `NativeIocsr` 的安全前提是运行在支持对应 LoongArch IOCSR 的环境，且 probe
  生命周期保证同一 controller 的配置操作被外部串行化。
- driver 内不引入 OS lock；`rdif_intc::Intc` 的外层设备锁串行化 task 控制面，
  CPU interface 仅访问独立寄存器或原子 snapshot。

## 兼容性与迁移

- compatible、ACPI/FDT wire format、默认寄存器地址、CPU0 route、domain kind、
  `IrqId` 和 parent-first probe 行为不变；
- `somehal` 删除重复寄存器定义、`irq_common`、LIO CPU-interface 文件和 PCH
  动态 route cache；
- 现有 LIO CPU-interface 与 PCH route 行为测试迁入新 crate，通过公共 API
  编译真实实现；
- 若集成验证失败，回滚整个 crate 接线即可恢复旧实现，不涉及持久数据或 firmware
  格式迁移。

## 验证计划

最低层 host 测试覆盖：

- EIO：MISC、NODEMAP、IPMAP、ROUTE、BOUNCE 初始化，enable bitmap，pending
  claim 与 W1 complete；
- PCH：vector/input 边界，ACPI identity，edge/level、polarity、MASK/HTVEC；
- LIO：parent map、route byte、初始化、enable snapshot、多 parent 同时 pending 时
  的 cascade claim 隔离、fallback parent 与无硬件 EOI；
- RDIF：三类 controller 的合法、空、越界、domain/config mismatch 路径。

集成与运行验证使用：

```bash
cargo test -p loongarch-intc-driver
cargo test -p loongarch-intc-driver --all-features
cargo test -p somehal --tests
cargo test -p rdrive --test fdt_priority_order
cargo fmt
cargo xtask clippy --package loongarch-intc-driver
cargo xtask clippy --package somehal
cargo xtask ktest qemu --workspace --exclude starry-kernel --exclude axvisor --arch loongarch64
cargo xtask arceos test qemu --arch loongarch64
cargo xtask starry test qemu --arch loongarch64
cargo xtask ktest qemu -p starry-kernel --arch loongarch64
```

JL-LSGD2K10 写测前后都需正常启动 Linux 并确认 ext4 无需人工 fsck。板上运行
Starry boot suite 与 `block-rw-bench`；后者的 AHCI 路径无 polling fallback，
因此通过可覆盖真实 LIOINTC IRQ 链。若写测后发现文件系统损坏，保存串口日志、
释放租约并停止交付；不得套用 OrangePi 专用 `fsckfix` 流程。

JL-LSGD2K10 的运行期仍使用 someboot 安装的 `TLBRENTRY`。其 LoongArch refill
walker 遇到空的中间目录项时必须立即写入两个全零的 `TLBRELO`，让硬件保留原始
load/store/fetch fault 类型；不得继续以物理地址 0 执行 `lddir`/`ldpte`，也不得
把包含故障虚拟页号的 `TLBREHI` 复用为 EntryLo。`test-calloc-mallocng` 的匿名页
首次写入检查与静态 musl `calloc` 覆盖该路径，实板 `block-rw-bench` 再覆盖真实
AHCI/LIOINTC I/O 链。
