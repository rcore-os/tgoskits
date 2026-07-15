# arm_vgic

`arm_vgic` 是一个 `no_std`、每 VM 独立的 Arm GICv3 控制器领域 crate。它按
GICv3 的物理结构建模 Distributor、每 vCPU Redistributor/虚拟 CPU Interface，
以及可选的 ITS/LPI 域。MMIO 映射、host IRQ 发现、guest memory、定时器和 vCPU
调度均留在 VMM 适配层，并通过受检 capability 接入。

当前只支持 GICv3 Group 1 Non-secure；不支持 GICv2、Secure Group 0/1、GICv4
vPE 和 nested virtualization。

## 构造流程

先用 `GicV3Config` 一次性校验 GICD、GICR、可选 GITS、vCPU 数量、LR 数量和
命令预算，再创建 `GicV3Controller`。设备连接中断源之前，必须用
`attach_vcpu` 为每个 vCPU 建立 `GicV3VcpuBinding` 和固定 affinity。

Emulated ITS 必须通过 `new_with_guest_memory` 提供 VM 级 `GuestMemory`
capability。ITS 只按受检 GPA 读取有预算上限的环形命令队列，不假设 GPA=HPA。

## 模式差异

- `Emulated`：GICD、GICR、SGI/PPI/SPI/LPI、CPU Interface 和 ITS 状态均按 VM
  保存；binding 在 guest 运行前后保存全部 LR/APR，并在 LR 耗尽时通过
  maintenance/refill 继续投递软件 pending。
- `Passthrough`：guest SPI 和 ITS translation 必须分别通过
  `bind_physical_spi`、`bind_physical_msi` 显式声明 host 资源和固定 vCPU
  affinity。SPI 绑定阶段只预留所有权，不修改 host 硬件；固定目标 vCPU 首次
  load 时，backend 才保存已释放 host 线路的 group、priority、trigger、route、
  pending、active 和 enable 快照，并只应用该 guest-owned SPI 的状态。binding
  save 时屏蔽物理线，销毁时恢复 host 快照。guest 对 GICD/GICR 混合寄存器的
  访问按所有权过滤，host-owned 位为 RAZ/WI；投递不回退到 LR，也不能直接访问
  共享 host ITS 寄存器。

Passthrough 私有中断也使用显式 mask。VMM 根据平台 capability 与 host/guest FDT
中的中断角色选择 SGI/PPI；binding load 只安装这些 guest-owned 私有状态并屏蔽
host-owned 线路，save 时恢复 host 快照，同时保留 guest 运行期间新 pending 的
host timer。VMM 必须在完整的 load/run/save 窗口中保持 host IRQ 屏蔽。

backend 必须校验平台 IRQ identity、affinity、地址、访问宽度和所有权。控制器在
锁内只生成投递动作，释放锁后才唤醒 vCPU 或调用 backend。

## 错误语义

所有可失败 API 返回 `VgicResult<T>`。`VgicError` 可区分非法 INTID、错误 INTID
类别、非法寄存器访问、状态转换、guest-memory 访问、ITS 命令或预算、资源缺失/
冲突、不支持能力和 backend 失败。架构规定的未知 RAZ/WI 寄存器读零/忽略写；
非法宽度、对齐、范围和所有权均显式报错。

## 破坏性 API 变化

新的 GICv3 API 直接替换旧 `Vgic`/GICv2、全局 host callback、全局 ITS/LPI
状态、crate 内定时器和手动硬件注入函数。集成层现在必须注册
`GicV3Controller`、绑定 vCPU，并让设备持有控制器创建的有线或 MSI endpoint。
虚拟 CNTP 定时器属于 VMM，每 vCPU 应持有自己的 PPI 中断线。

## 验证

```bash
cargo fmt --all --check
cargo clippy -p arm_vgic --all-targets --all-features -- -D warnings
cargo test -p arm_vgic --all-features
RUSTDOCFLAGS="-D warnings" cargo doc -p arm_vgic --all-features --no-deps
```

本项目使用 Apache-2.0 许可证。
