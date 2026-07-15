# ax-percpu

面向 `no_std` 内核的架构无关 per-CPU 布局与 typed access。

`ax-percpu` 只负责模板布局、symbol offset、不可变 runtime area 注册，以及 current/remote 地址计算。所有架构寄存器指令集中在仅依赖 value-only `trait-ffi` 的叶子 crate `ax-cpu-local`。

每个 area 以固定 `CpuAreaPrefixV2` 开始。final-high 平台入口先一次构造连续 layout，再由平台在 CPU online 前绑定完整的 `CpuBindingV1`：

```rust,ignore
unsafe {
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
    ax_percpu::initialize_layout(init)?;
}

let cpu = ax_percpu::CpuIndex::try_from(cpu_id)?;
let area = ax_percpu::area(cpu)?;
unsafe { ax_cpu_local::raw::install_binding(area.binding())? };
```

外部分配的 area 起初只是 raw storage。`def_percpu` 在 `.percpu.storage` 生成 `MaybeUninit` storage 与 typed initializer registration；final-high 初始化先校验完整相对 descriptor 表，再用 `ptr::write` 为每个 CPU 独立构造对象，不复制任意 Rust 对象的模板字节。

安全 current access 必须接收 `BoundCpuPin`：先从 IRQ/抢占 guard 借出迁移用 `CpuPin`，再通过 `bound_current` 将平台发布的完整 binding 与 frozen layout、固定 prefix 逐项匹配。primitive 使用对应 `Atomic*` 的 Relaxed 操作，避免 hard IRQ 重入产生 data race；对象仅在 `T: Sync` 时提供 `with_current_ref`，可变对象访问必须显式 `unsafe`，因为绑定与迁移证明都不能证明别名排他。

固定 header 的最小对齐是 64 字节，但这不是 per-CPU 对象的对齐上限。`def_percpu` 为每个真实 storage 生成普通 Rust alignment descriptor；链接器按 `.percpu` 的实际最大对齐布置模板 base 和每个 area stride，layout 注册时再校验 descriptor 最大值。包括大于页大小的合法 `repr(align(...))` 类型都不需要架构专用访问指令。

默认 linked storage、平台外部分配的 `custom-base`、单核 `sp-naive` 共用同一变量 API。

内核平台使用 `custom-base` 时还必须启用 `linked-template`。链接脚本必须在所有普通 per-CPU 输入 section 之后保留 `.percpu_end`，并单独 `KEEP(*(.ax_percpu.align))`；随后由 `ax-cpu-local` 提供 Rust 对象拥有的精确模板边界，由 alignment descriptor 表提供动态分配对齐，不依赖 target triple 猜测或架构专用 load/store。

非内核 host consumer 使用 `custom-base` 时可以链接只检查源码的测试；若实际执行 layout 或 value access，则必须显式提供 host storage fixture，否则会明确失败。单核模型测试使用 `sp-naive`；需要模拟多个 area 时使用 `host-test` 及对应 linker fixture。

启用 `host-test` 后，每个 integration test 二进制仍须显式提供自己的 `CpuLocalPlatformV1` fake provider；不存在可能与真实 OS provider 冲突的默认实现。host register fixture 是 thread-local；每个 test-harness 或模拟 CPU 的线程都必须在访问前安装自己的完整 area binding，不存在进程级 fallback。

```bash
cargo test -p ax-percpu --features host-test,non-zero-vma
cargo test -p ax-percpu --features host-test,non-zero-vma,custom-base
cargo test -p ax-percpu --features host-test,non-zero-vma,sp-naive
cargo xtask clippy --package ax-percpu
```

采用 Apache-2.0 许可证。
