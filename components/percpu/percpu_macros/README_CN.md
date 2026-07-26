# ax-percpu-macros

`ax-percpu` 使用的过程宏。应用应导入 `ax_percpu::def_percpu`，而不是直接依赖
本 crate。

`#[def_percpu]` 为最终镜像生成三类元数据：

- `.percpu.template.storage` 中的未初始化类型化存储；
- `.percpu.init` 中的类型化构造注册项；
- `.percpu.align` 中的对齐描述符。

布尔值和整数使用匹配的原子表示；其他对象保留 Rust 类型，并在每个最终运行时 CPU
区域中各构造一次。生成的访问包装器只计算模板相对偏移，当前/远端访问校验统一交给
`ax-percpu`。

本 crate 不定义 Cargo feature，也不拥有运行时存储后端。链接布局、动态分配、
类型化初始化、CPU pin 与寄存器所有权分别属于 `ax-percpu`、平台和 `cpu-local`。

```bash
cargo test -p ax-percpu-macros
cargo xtask clippy --package ax-percpu-macros
```

采用 Apache-2.0 许可证。
