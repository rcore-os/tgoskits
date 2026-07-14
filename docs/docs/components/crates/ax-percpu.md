# `ax-percpu`

> 路径：`components/percpu/percpu`
> 分层：通用 CPU-local 语义层（`no_std`）

`ax-percpu` 管理 per-CPU 模板布局、符号 offset、CPU area 注册，以及 current/remote 地址计算。它不包含任何架构汇编，也不直接读写 GS、TPIDR、`sscratch`、`r21` 等寄存器；这些最小指令统一属于零依赖叶子 crate `ax-cpu-local`。

## 分层与所有权

```text
ax-cpu-local
  ├─ CpuAreaHeader / CpuPin / CpuIndex
  └─ 四架构 CPU anchor 的最小 inline asm

ax-percpu
  ├─ .percpu 模板与 symbol offset
  ├─ PerCpuLayoutV1 / PerCpuArea / BoundCpuPin
  ├─ current = area base + symbol offset
  └─ remote = runtime_base + cpu * stride + offset
```

每个 area 从固定的 `CpuAreaPrefix` 开始：第一条 cache line 是只读身份 `CpuAreaHeader`，第二条 cache line 是 trap entry scratch。header 保存 `self_base`、relocation、`CpuIndex`、generation 与 cookie。链接脚本必须把 `.percpu.000.header` 放在模板 offset 0，并保证至少 64 字节对齐。64 字节只是 header 的下限，不是变量的上限；宏在 `.ax_percpu.align` 中记录每个 storage 的实际对齐，链接器以 `MAX(64, ALIGNOF(.percpu))` 对齐模板、runtime base 和 stride，Rust 注册逻辑再次核对两份元数据。

CPU anchor 的归属如下：

| 架构 | CPU-owned anchor |
| --- | --- |
| x86_64 | kernel GS base 指向 `CpuAreaHeader` |
| AArch64 | TPIDR_EL1/EL2 指向 `CpuAreaHeader` |
| RISC-V | `sscratch` 指向 `CpuAreaHeader` |
| LoongArch | `r21` 与 KS3 保存 relocation mirror |

anchor 永远不进入任务上下文。RISC-V `gp` 是标准 psABI global pointer，`tp` 是任务 TLS；LoongArch 用户 `r21` 只在 user trap frame 中保存和恢复。

## 初始化协议

平台先准备连续、CPU-lifetime 的区域，并一次注册：

```rust,ignore
unsafe {
    ax_percpu::install_layout(ax_percpu::PerCpuLayoutV1 {
        runtime_base,
        area_stride,
        area_count,
        flags: 0,
})?;
}
```

`install_layout` 是 `unsafe`，因为数值校验无法证明 runtime range 已映射、可读写且持续到关机；平台调用者必须提供该存储生命周期保证。

每个 CPU 在 IRQ/trap 尚不可进入、调度尚未开放时执行唯一绑定：

```rust,ignore
let cpu = ax_percpu::CpuIndex::try_from(cpu_id)?;
let area = ax_percpu::area(cpu)?;
unsafe { ax_percpu::bind_current(area)? };
```

`bind_current` 在写 header 或架构寄存器前完成所有可恢复校验；一旦进入 commit，后续 verify 失败属于不可回滚的架构不变量并立即 fatal，不能把已经发布的 CPU anchor 伪装成普通 `Err` 返回。

相同 layout 的重复注册是幂等的，冲突 layout 会返回错误。`flags` 当前必须为零；stride、对齐、模板大小、CPU 数量和地址溢出都会被校验。layout 与 area 持续到关机，本版本不支持 CPU hotplug 或重绑定。

ArceOS 动态平台的唯一 binder 是 `axplat-dyn` platform entry。`ax-plat` 和 `ax-runtime` 只能验证 header 并发布自己的 per-CPU 字段，不得再次写 CPU anchor。

## 访问 API

`#[def_percpu]` 生成统一的 `PerCpu<T, Symbol, AccessKind>` 描述符；`custom-base` 不再生成另一套包装类型。

`CpuPin` 只证明操作期间不会迁移，不证明架构 anchor 已指向已安装 area。安全 current access 必须先调用 `bound_current(&CpuPin)`，得到同时借用迁移证明并验证 layout、stride、header index/generation/cookie 的 `BoundCpuPin`。`PreemptGuard`、`IrqGuard` 和 `PreemptIrqGuard` 只能借出 `CpuPin`，不能直接构造更强的绑定证明；两种 pin 都不证明 IRQ 排他或可变别名排他。

```rust,ignore
#[ax_percpu::def_percpu]
static CPU_NUMBER: usize = 0;

fn publish(pin: &ax_percpu::CpuPin, value: usize) -> Result<(), ax_percpu::PerCpuError> {
    let bound = ax_percpu::bound_current(pin)?;
    CPU_NUMBER.write_current(&bound, value);
    assert_eq!(CPU_NUMBER.read_current(&bound), value);
    Ok(())
}
```

- `bool/u8/u16/u32/u64/usize` 使用对应 `Atomic*` 模板，安全 `read_current/write_current` 是 Relaxed atomic，hard IRQ 重入不会产生 Rust data race。
- 对象只在 `T: Sync` 时提供 HRTB `with_current_ref`，闭包不能安全导出临时引用。
- 对象可变访问仅提供 `unsafe with_current_mut_raw`/raw pointer API；调用者必须额外证明 IRQ、嵌套调用和 remote access 均不产生别名。
- remote access 接收 typed `CpuIndex`，通过已安装 layout 做 O(1) 计算；产生引用的 remote API 是 `unsafe`。
- `repr(align(...))` 可高于 cache line 或 page；平台分配器必须使用链接器发布的实际 template alignment，不得把 64B header alignment 或 4K page size 当成 symbol alignment 上限。

旧的 `init_percpu_reg`、`read_percpu_reg`、`write_percpu_reg`、整数式 `percpu_area_base` API 已删除。early boot、trap prologue 和 LockRuntime 只能使用清楚标注且局部封装的 raw API。

## Feature

- `custom-base`：区域由平台外部分配；仍使用相同 layout、area 和变量 API。
- `sp-naive`：单核退化模型，不读取架构 anchor。
- `host-test`：host 线程本地 anchor 与测试存储。
- `non-zero-vma`：允许 host/test 的模板不是零 VMA。
- `arm-el2`：AArch64 使用 TPIDR_EL2。

## 验证重点

- `ax-percpu` 与 `percpu_macros` 中不得出现 `asm!`/`global_asm!` 或架构寄存器名。
- `ax-percpu` 不依赖 `ax-task`、`ax-runtime`、`ax-hal` 或具体平台。
- 无 `BoundCpuPin` 不能调用安全 current accessor；它必须借用仍存活的 `CpuPin`，且二者与 guard 都不可跨线程。
- normal、`custom-base`、`sp-naive` 三种模式必须共享同一变量 API。
- 链接脚本必须分别保留 `.percpu` 模板与 `.ax_percpu.align` descriptor 表；runtime base 和 stride 必须满足 descriptor 最大值，且 linker `ALIGNOF(.percpu)` 与 Rust descriptor 最大值必须一致。
- 四架构启动必须在 CPU online 前完成 header 初始化与 anchor 验证。
