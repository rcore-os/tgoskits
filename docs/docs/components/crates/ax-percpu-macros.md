# `ax-percpu-macros`

> 路径：`components/percpu/percpu_macros`
> 分层：per-CPU 编译期布局生成层

该 crate 只实现 `#[def_percpu]` 的架构无关代码生成。上层应通过 `ax_percpu::def_percpu` 使用它，不应直接依赖本 crate。

## 生成内容

`#[def_percpu] static VALUE: T = INIT;` 展开为：

1. `.percpu.storage` 中的 `MaybeUninit<Storage>` raw storage；
2. 保留在 `.ax_percpu.init` 的 typed initializer registration；
3. 仅暴露相对 template offset 的私有 symbol provider；
4. 统一的 `ax_percpu::PerCpu<T, Symbol, AccessKind>` 零大小描述符。

普通对象的 `Storage` 是 `T`。`bool/u8/u16/u32/u64/usize` 的 `Storage` 是对应 `Atomic*`，并选择 `PrimitiveAccess`；这使 safe primitive access 能用 Relaxed atomic 覆盖 hard IRQ 重入。其它类型选择 `ObjectAccess`。所有 storage 起初都是 `MaybeUninit`，final-high 初始化为每个 CPU 用 typed `ptr::write` 独立构造值，不复制 Rust 对象字节。

宏只生成：

- 变量与 section；
- typed initializer 与明确的 unsafe registration；
- symbol provider 类型；
- final image 中 `addr_of!` 得到的 loaded symbol 地址；
- 相对 `CpuAreaHeader` 模板起点的 offset；
- 调用 `ax-percpu` 通用地址函数的 provider。

loaded symbol 地址只在 final relocation 后转换为经过 checked subtraction 的 offset，不作为公共 VMA API 暴露。

宏不生成 load/store 指令，不读取架构寄存器，也不按 `target_arch` 分叉。`custom-base` 与默认模式使用完全相同的展开；只有 `sp-naive` 将 symbol 地址直接作为单核地址。

## 强制边界

- 不得包含 `asm!`、`global_asm!`。
- 不得出现 GS、TPIDR、`sscratch`、`gp`、`r21` 等寄存器协议。
- 不得生成隐式关闭抢占的 accessor；安全 current accessor 必须显式接收由 `CpuPin` 验证得到的 `BoundCpuPin`。
- 不得为对象生成 safe mutable reference/closure。
- 不得为 primitive 和对象复制两套 wrapper API。

架构寄存器编码只允许在 `ax-cpu-local` allowlist 文件中；layout typed initialization、current/remote 地址计算和 aliasing 规则属于 `ax-percpu`，CPU register binding 属于 platform。

## 验证

修改宏后至少检查：

- normal、`custom-base`、`sp-naive`、`tls` 四种 ax-percpu 测试；
- source audit 精确验证 macro storage 位于 `.percpu.storage`，不得回退到 `.percpu.data`；
- primitive atomic storage 与对象 `T: Sync` 门禁；
- source audit 中零架构汇编和寄存器名；
- 四架构 `axplat-dyn`/`ax-cpu` 交叉构建。
