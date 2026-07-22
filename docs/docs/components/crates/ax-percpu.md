# ax-percpu

`ax-percpu` 提供架构无关、类型化的 per-CPU 布局、运行时初始化和变量访问。
架构寄存器读写不在本 crate 中，而由 `cpu-local` 统一拥有。

## 动态-only 模型

最终 ELF 只保留一份布局模板，不按 CPU 数复制数据。平台启动流程必须按以下顺序
完成：

1. 从 `.percpu.template` 取得唯一模板的地址与大小。
2. 根据 `.percpu.align` 求出最终区域对齐和 stride。
3. 为所有 CPU 分配持续到关机的可写区域。
4. 用 `PerCpuLayoutInitV2` 一次校验完整布局。
5. 在每个最终地址构造 `CpuAreaPrefixV2` 和全部类型化变量。
6. 冻结布局，然后才允许平台安装 `PerCpuArea::binding()`。

不存在链接期运行时 alias、静态 CPU 区域或链接脚本按 `SMP`/CPU 数展开的路径。
因此 CPU 数只影响运行时分配，不影响 `.percpu.template` 大小。

## 链接契约

最终输出节固定为：

| 输出节 | 内容 |
| --- | --- |
| `.percpu.template` | 固定头、所有类型化槽位和结束哨兵 |
| `.percpu.init` | 最终重定位后解析的类型化构造描述符 |
| `.percpu.align` | 每个槽位的真实对齐要求 |

宏输入节为 `.percpu.template.header`、`.percpu.template.storage`、
`.percpu.template.end`。边界统一使用 `__PERCPU_*` 和 `__CPU_LOCAL_*`；
链接脚本只接收这些精确节名。

someboot 通过值类型 C ABI 调用：

- `__percpu_image_register_mode_v1`
- `__percpu_initialize_layout_v2`

## 公共接口

- `PerCpuLayoutV1`：连续运行时区域的 base、stride 与 area count。
- `PerCpuLayoutInitV2`：增加 ABI、寄存器模式、host level、generation 和 cookie。
- `PerCpuArea`：一个已初始化 CPU 区域的只读描述符及 binding。
- `BoundCpuPin`：把调用者的 `CpuPin` 强化为已验证的当前区域能力。
- `PerCpuError`：可匹配的布局、初始化与绑定错误。
- `initialize_layout`、`layout`、`area`、`bound_current`：初始化和查询入口。

典型访问：

```rust,no_run
#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

fn set_cpu_id(pin: &ax_percpu::CpuPin, value: usize) {
    let bound = ax_percpu::bound_current(pin).unwrap();
    CPU_ID.write_current(&bound, value);
}
```

安全访问必须持有 `BoundCpuPin`。原始标量使用匹配的原子类型，避免中断重入造成
Rust data race；对象在每个最终区域只构造一次。对象可变访问仍为 `unsafe`，因为
pin 只能证明不迁移，不能单独证明引用排他。

## Feature 与测试

crate 只提供 `host-test` feature。`host_test::initialize(NonZeroU32)` 为测试进程
分配生命周期内的动态区域并执行同一套类型化初始化。每个模拟 CPU 线程都必须显式
安装 `area(cpu).binding()`，不存在进程级当前 CPU fallback。

```bash
cargo test -p ax-percpu --features host-test
cargo xtask clippy --package ax-percpu
```

平台接入还必须验证不同加载偏移、SMP 数量和目标架构下的 ELF 节、重定位与启动路径。
