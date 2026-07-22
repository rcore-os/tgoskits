# ax-percpu

面向 `no_std` 内核的类型化 per-CPU 布局、初始化与访问组件。

`ax-percpu` 只保留动态实现：最终 ELF 仅携带一份布局模板，平台为每个 CPU
分配可写运行时区域，并在任何 CPU 绑定前完成整体初始化。架构寄存器的所有权由
独立的 `cpu-local` crate 提供。

## 运行时契约

初始化顺序固定为：

1. 链接器只保留一份 `.percpu.template`，以及 `.percpu.init`、
   `.percpu.align` 两张描述符表。
2. 平台为所有 CPU 区域分配持续到关机的存储。
3. `initialize_layout(PerCpuLayoutInitV2)` 一次校验完整几何和描述符。
4. 在最终地址分别构造每个 `CpuAreaPrefixV2` 和每个类型化值。
5. 冻结布局后，平台才能安装 `area(cpu).binding()`。

不存在链接期运行时布局或按 CPU 数静态复制，因此运行时 CPU 数变化不会改变 ELF
模板大小。

链接输出节仅有：

- `.percpu.template`
- `.percpu.init`
- `.percpu.align`

宏生成的存储位于 `.percpu.template.storage`；固定头和结束哨兵分别位于
`.percpu.template.header`、`.percpu.template.end`。边界符号统一使用
`__PERCPU_*` 和 `__CPU_LOCAL_*`。

## 类型化访问

```rust,no_run
#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

fn set_cpu_id(pin: &ax_percpu::CpuPin, cpu_id: usize) {
    let bound = ax_percpu::bound_current(pin).expect("CPU area must be bound");
    CPU_ID.write_current(&bound, cpu_id);
    assert_eq!(CPU_ID.read_current(&bound), cpu_id);
}
```

原始标量使用匹配的原子存储。对象初始化表达式通过类型化描述符保留，并在每个最终
运行时区域独立构造一次；实现不会复制任意 Rust 对象的模板字节。

安全的当前 CPU 访问必须使用 `BoundCpuPin`。它借用调用者的迁移 pin，并将实时
绑定与已冻结布局逐项校验。对象可变访问仍为 `unsafe`，因为 CPU pin 本身不能证明
Rust 别名排他性。

## Host 测试

crate 只提供一个 feature：`host-test`。`host_test::initialize(NonZeroU32)`
分配进程生命周期内的动态区域并完成一次初始化；每个模拟 CPU 的线程仍须显式安装
自己的 `area(cpu).binding()`。

```bash
cargo test -p ax-percpu --features host-test
cargo test -p ax-percpu-macros
cargo xtask clippy --package ax-percpu
```

采用 Apache-2.0 许可证。
