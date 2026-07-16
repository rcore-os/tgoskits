# `ax-percpu`

> 路径：`components/percpu/percpu`
> 分层：通用 CPU-local 语义层（`no_std`）

`ax-percpu` 管理 per-CPU template、symbol offset、final-high typed initialization、冻结后的 area layout，以及 current/remote 地址计算。它不包含架构汇编，也不直接读写 GS、TPIDR、`sscratch`、`r21` 等寄存器；这些最小指令只允许位于 `ax-cpu-local`，该叶子 crate 仅依赖 value-only `trait-ffi` 边界。

## 分层与所有权

```text
ax-cpu-local
  ├─ CpuAreaPrefixV2 / CpuPin / CpuIndex / CpuBindingV1
  ├─ CpuLocalPlatformV1 value-only trait-ffi
  └─ allowlist 中的四架构寄存器 primitive

ax-percpu
  ├─ .percpu template、typed initializer table 与 symbol offset
  ├─ PerCpuLayoutV1 / PerCpuArea / BoundCpuPin
  ├─ current = platform binding.area_base + symbol offset
  └─ remote = runtime_base + cpu * stride + offset
```

每个 area 从固定 192 字节的 `CpuAreaPrefixV2` 开始：

- cache line 0：不可变 `CpuAreaHeader`，保存 ABI version、register mode、host level、CPU index、generation、direct area base、boot-thread pointer 与 cookie；
- cache line 1：`CpuRuntimeAnchor`，保存 current-thread slot、kernel continuation、user frame 与 trap scratch；
- cache line 2：永久 `BootThreadHeader`。

header 不保存 relocation。链接脚本必须把 `.percpu.000.header` 放在 template offset 0，并让 prefix 至少 64 字节对齐。64 字节不是普通 per-CPU 对象的对齐上限；宏在 `.ax_percpu.align` 中发布每个 storage 的真实对齐，链接器按 `MAX(64, ALIGNOF(.percpu))` 对齐 template、runtime base 和 stride，Rust 初始化逻辑再核对 descriptor 最大值。

CPU-owned anchor 永远不进入任务上下文，`ax-percpu` 也不假设某一种寄存器编码。RISC-V 的 `gp` 始终是标准 psABI global pointer；LinuxCurrent 模式由 pinned `CurrentThreadHeader` 的 `tp` 恢复 area，正常内核态 `sscratch=0`，trap 仅在 entry/return handshake 中借用它；UnikernelTls 模式才由 `sscratch` 保存 area、`tp` 保存 TLS。LoongArch 的 kernel `r21` 与 KS3 保存 direct area base，用户 `r21` 只由 user trap context 保存和恢复。

## final-high typed initialization

early boot 只分配、清零并映射 raw area storage，不复制 template 中任意 Rust 对象。完成最终镜像 relocation 后，唯一 final-high 入口构造所有 area：

```rust,ignore
let layout = ax_percpu::PerCpuLayoutV1 {
    runtime_base,
    area_stride,
    area_count,
    flags: 0,
};
let init = ax_percpu::PerCpuLayoutInitV2::new(
    layout,
    generation,
    cookie,
    register_mode,
    host_level,
);

unsafe { ax_percpu::initialize_layout(init)? };
```

`#[def_percpu]` 为每个对象在 `.percpu.storage` 生成 `MaybeUninit<Storage>`，并在 `.ax_percpu.init` 生成 registration。`initialize_layout` 在第一次 destination write 前校验全部 prefix facts、descriptor 范围、size、alignment、overflow 与 pairwise overlap；通过后才在每个最终 area 地址用 typed initializer 和 `ptr::write` 独立构造值。它不会把一个 Rust 对象的字节复制到另一个 allocation。

descriptor/registration constructor 是隐藏的 `unsafe const fn`。其 safety contract 要求 thunk 属于同一 final image、终身有效、每次返回完全相同的 storage/function descriptor；否则“先验证全部记录、后写入”的两阶段协议无法成立。

layout 初始化和发布都只能成功一次，持续到关机；`flags` 当前必须为零。本版本不支持 CPU hotplug、运行期重绑定或动态卸载。

## 平台绑定边界

`ax-percpu` 不安装架构寄存器。它只向 platform binder 提供完整 value-only binding：

```rust,ignore
let cpu = ax_percpu::CpuIndex::try_from(cpu_id)?;
let area = ax_percpu::area(cpu)?;

// Only the offline, IRQ/trap-excluded platform binder may call this leaf API.
unsafe { ax_cpu_local::raw::install_binding(area.binding())? };
```

真实 OS 必须实现唯一 `CpuLocalPlatformV1` provider；普通 consumer 只调用 `ax_cpu_local::platform` client facade。`ax-percpu` 没有 `bind_current`、`InstalledPerCpuArea` 或 raw register accessor，避免语义层再次成为第二个 binder。CPU 完成 prefix 初始化和平台绑定前不得 online。

host integration test 也不获得默认 provider。每个独立测试二进制必须显式提供自己的 fake `CpuLocalPlatformV1`，避免测试符号与真实 OS provider 冲突。

## 访问 API

`CpuPin` 只证明操作期间不会迁移，不证明 CPU area 已绑定。安全 current access 必须先调用 `bound_current(&CpuPin)`；该函数消费 platform facade 返回的完整 binding，按 CPU index 找到 frozen layout area，逐项匹配 ABI/mode/host/generation/base/boot-thread/cookie，再校验不可变 prefix，最终返回借用原 pin 的 `BoundCpuPin`。

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

- `bool/u8/u16/u32/u64/usize` 使用对应 `Atomic*` storage；安全 `read_current/write_current` 是 Relaxed atomic，hard IRQ 重入不会产生 Rust data race。
- 对象只在 `T: Sync` 时提供 HRTB `with_current_ref`，闭包不能安全导出临时引用。
- 对象可变访问仅提供 `unsafe with_current_mut_raw`/raw pointer API；调用者必须额外证明 IRQ、嵌套调用和 remote access 不会形成别名。
- `PerCpuArea::prefix/runtime_anchor` 提供 typed shutdown-lifetime remote view；`BoundCpuPin::prefix/runtime_anchor` 提供 pinned current view，不需要 consumer cast runtime address。
- remote access 接收 typed `CpuIndex`，通过 frozen layout 做 O(1) 地址计算；产生引用的 remote API 仍是 `unsafe`。
- `repr(align(...))` 可以高于 cache line 或 page；platform allocator 必须服从链接器发布的真实 template alignment。

## Feature

- `custom-base`：area storage 由平台外部分配；变量 API 不变。
- `linked-template`：显式使用最终内核镜像保留的 template bounds。
- `sp-naive`：单核退化模型，不查询 platform binding。
- `host-test`：启用 host register/storage fixture，但不提供默认 trait-ffi provider。
- `non-zero-vma`：允许 host/test template 使用非零 VMA。
- `tls`：选择 final-image `UnikernelTls` mode，并转发到 `ax-cpu-local/tls`。

不存在 `arm-el2` feature。AArch64 host level 必须在 final-high 阶段读取 live `CurrentEL`，不能由 Cargo feature 猜测。

## 验证重点

- `ax-percpu` 与 `percpu_macros` 中必须为零 `asm!`/`global_asm!`、零架构寄存器名；`percpu_macros/src/arch.rs` 不得存在。
- `ax-percpu` 不依赖 `ax-task`、`ax-runtime`、`ax-hal` 或具体平台；`ax-cpu-local` 只能依赖 `trait-ffi`。
- 无 `BoundCpuPin` 不能调用安全 current accessor；`CpuPin`、`BoundCpuPin` 与 guard 都不可跨线程。
- normal、`custom-base`、`sp-naive` 与 `tls` 组合共享同一变量 API。
- linker 必须分别保留 `.percpu`、`.ax_percpu.init` 与 `.ax_percpu.align`，prefix 必须位于 offset 0。
- final-high 初始化前不得访问 area；平台绑定完成前 CPU 不得 online。
- x86 raw ELF entry 必须在任何 Rust frame 前完成 relocation，并用三个真实 runtime load bias 执行验证。
