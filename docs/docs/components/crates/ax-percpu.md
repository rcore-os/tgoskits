# ax-percpu

`ax-percpu` 提供架构无关、类型化的 per-CPU 布局、运行时初始化和变量访问。
架构寄存器读写不在本 crate 中，而由 `cpu-local` 统一拥有。

## 动态-only 模型

最终 ELF 只保留一份布局模板，不按 CPU 数复制数据。平台启动流程必须按以下顺序
完成：

1. 从 `.percpu.template` 取得唯一模板的地址与大小。
2. 根据 `.percpu.align` 求出最终区域对齐和 stride。
3. 为所有 CPU 分配持续到关机的可写区域。
4. 用 `initialize_layout(PerCpuRegion)` 在第一次目标写入前校验完整布局。
5. 在每个最终地址构造 `CpuAreaPrefix` 和全部类型化变量。
6. 冻结布局，然后平台才可从 `PerCpuArea::cpu_area()` 取得 `CpuAreaRef` 并在
   offline CPU 上安装。

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

someboot 只通过一个无版本、纯标量 C ABI 调用
`__percpu_initialize_layout(runtime_base, area_stride, area_count)`。寄存器模式由最终
镜像的 feature graph 决定，不在运行时重复传递。

## 公共接口

- `PerCpuRegion`：平台拥有的 base、stride 与非零 area count。
- `PerCpuLayout`：初始化完成后冻结的进程级布局。
- `PerCpuArea`：一个已初始化 CPU 区域的只读描述符。
- `CpuPin<'scope>`：不可逃逸、不可跨线程的当前 CPU 能力。
- `ExclusiveCpu<'pin>`：额外证明本地 IRQ/重入和冲突远端访问已排除的可变访问能力。
- `PerCpuError`：可匹配的布局、初始化与绑定错误。
- `initialize_layout`、`layout`、`area`、`current_area`：初始化和查询入口。

典型访问：

```rust,no_run
#[ax_percpu::def_percpu]
static CPU_ID: usize = 0;

fn set_cpu_id(pin: &ax_percpu::CpuPin<'_>, value: usize) {
    ax_percpu::current_area(pin).unwrap();
    CPU_ID.write_current(pin, value);
}
```

原始标量使用匹配的原子类型；对象在每个最终区域只构造一次。`T: Sync` 对象可在
`CpuPin` 下共享访问，本地可变对象必须通过 `ExclusiveCpu` 的非逃逸回调访问。远端
访问显式接收 `PerCpuArea` 并由调用者负责同步。

| 操作 | 必要保护 |
| --- | --- |
| 原子标量 | 禁止迁移；允许本地 IRQ |
| `T: Sync` 共享对象 | 禁止迁移；对象自行同步 |
| 本地可变对象 | 禁止迁移、IRQ/重入和冲突远端访问 |
| 调度切换 | IRQ 关闭、禁止迁移，并消费事务 token |
| vCPU 运行 | 禁止迁移；退出汇编在 Rust 前恢复 host 寄存器 |
| CPU 区域初始化 | CPU offline，区域独占且尚无 live Rust 值 |

## Feature 与测试

crate 只提供 `host-test` feature。`host_test::initialize(NonZeroU32)` 为测试进程
分配生命周期内的动态区域并执行同一套类型化初始化。每个模拟 CPU 线程都必须显式
安装 `area(cpu).cpu_area()`，不存在进程级当前 CPU fallback。

```bash
cargo test -p ax-percpu --features host-test
cargo xtask clippy --package ax-percpu
```

平台接入还必须验证不同加载偏移、SMP 数量和目标架构下的 ELF 节、重定位与启动路径。
