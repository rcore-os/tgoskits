# ax-percpu

面向 `no_std` 内核的架构无关 per-CPU 布局与 typed access。

`ax-percpu` 只负责模板布局、symbol offset、不可变 runtime area 注册，以及 current/remote 地址计算。所有架构寄存器指令集中在零依赖叶子 crate `ax-cpu-local`。

每个 area 以固定 `CpuAreaHeader` 开始。平台先一次注册连续 layout，再在 CPU online 前绑定当前 area：

```rust,ignore
unsafe {
    ax_percpu::install_layout(ax_percpu::PerCpuLayoutV1 {
        runtime_base,
        area_stride,
        area_count,
        flags: 0,
    })?;
}

let cpu = ax_percpu::CpuIndex::try_from(cpu_id)?;
let area = ax_percpu::area(cpu)?;
unsafe { ax_percpu::bind_current(area)? };
```

外部分配的 area 必须在 bind 前完整复制链接模板；仅清零任意对象存储不能保证 Rust 位型有效。

安全 current access 必须接收 `BoundCpuPin`：先从 IRQ/抢占 guard 借出迁移用 `CpuPin`，再通过 `bound_current` 校验原始 CPU anchor、已注册 layout 和固定 header。primitive 使用对应 `Atomic*` 的 Relaxed 操作，避免 hard IRQ 重入产生 data race；对象仅在 `T: Sync` 时提供 `with_current_ref`，可变对象访问必须显式 `unsafe`，因为绑定与迁移证明都不能证明别名排他。

固定 header 的最小对齐是 64 字节，但这不是 per-CPU 对象的对齐上限。`def_percpu` 为每个真实 storage 生成普通 Rust alignment descriptor；链接器按 `.percpu` 的实际最大对齐布置模板 base 和每个 area stride，layout 注册时再校验 descriptor 最大值。包括大于页大小的合法 `repr(align(...))` 类型都不需要架构专用访问指令。

默认 linked storage、平台外部分配的 `custom-base`、单核 `sp-naive` 共用同一变量 API。

内核平台使用 `custom-base` 时还必须启用 `linked-template`。链接脚本必须在所有普通 per-CPU 输入 section 之后保留 `.percpu_end`，并单独 `KEEP(*(.ax_percpu.align))`；随后由 `ax-cpu-local` 提供 Rust 对象拥有的精确模板边界，由 alignment descriptor 表提供动态分配对齐，不依赖 target triple 猜测或架构专用 load/store。

非内核 host consumer 使用 `custom-base` 时可以链接只检查源码的测试；若实际执行 layout 或 value access，则必须显式提供 host storage fixture，否则会明确失败。单核模型测试使用 `sp-naive`；需要复制多个 area 时使用 `host-test` 及对应 linker fixture。

启用 `host-test` 后，第一次显式安装的 anchor 会成为新建 host 线程继承的不可变 bootstrap fallback；模拟其他 CPU 的线程必须在访问前显式绑定自己的 area。

```bash
cargo test -p ax-percpu --features host-test,non-zero-vma
cargo test -p ax-percpu --features host-test,non-zero-vma,custom-base
cargo test -p ax-percpu --features host-test,non-zero-vma,sp-naive
cargo xtask clippy --package ax-percpu
```

采用 Apache-2.0 许可证。
