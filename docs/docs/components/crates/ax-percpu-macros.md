# ax-percpu-macros

`ax-percpu-macros` 实现 `ax_percpu::def_percpu`，只负责生成类型化槽位和描述符，
不拥有运行时存储或架构寄存器。

## 宏展开契约

对每个 `#[def_percpu] static X: T = expr;`，宏生成：

1. `.percpu.template.storage` 中的 `MaybeUninit<Storage>` 槽位。
2. `.percpu.init` 中的描述符 thunk 和类型化构造函数。
3. `.percpu.align` 中的真实 `align_of::<Storage>()`。
4. 一个只暴露类型化访问方法的零大小 wrapper。

`bool`、`u8`、`u16`、`u32`、`u64`、`usize` 使用对应 `Atomic*` 作为
`Storage`；其他类型直接保留 `T`。所有初始化描述符会在任意写入前整体校验范围、
对齐与重叠，然后在每个最终 CPU 区域用 `ptr::write` 独立构造一次。

符号偏移只在最终重定位后按
`storage_address - __CPU_LOCAL_AREA_PREFIX` 解析。宏不会生成架构汇编，也不会读取
GS、TPIDR、`sscratch` 或 r21；这些寄存器只能由 `cpu-local` 后端处理。

## 边界

- 本 crate 不定义 Cargo feature。
- 不存在多种存储后端或静态运行时布局。
- 链接脚本必须精确保留 `.percpu.template.storage`、`.percpu.init`、
  `.percpu.align`，并由 `ax-percpu` 校验最终布局。
- 应用使用 `ax_percpu::def_percpu`，不直接依赖本 crate 的内部生成项。

```bash
cargo test -p ax-percpu-macros
cargo xtask clippy --package ax-percpu-macros
```
