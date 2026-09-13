# 内核 TLS 与用户态模式

## 1. 模式与兼容性

最终映像只能采用一种内核寄存器所有权。Cargo 的 feature 合并可能同时启用 `tls` 和 `uspace`；此时采用用户态模式，避免内核 TLS 与当前任务指针争用寄存器。

### 1.1 功能选择

`ax-cpu`、`cpu-local`、`ax-hal`、`ax-runtime` 和 `someboot` 的 `build.rs` 从 Cargo 传入的 feature 派生 `kernel_tls` cfg。只有 `tls` 开启且 `uspace` 关闭时输出该 cfg；源码使用 `cfg(kernel_tls)` 或其补集选择已有实现。此 cfg 是包内实现细节，不是用户配置入口。

| Cargo features | 内核 TLS | 当前任务与用户态行为 |
| --- | --- | --- |
| 均关闭 | 关闭 | 保持原有模式 |
| 仅 `tls` | 开启 | 保持原有内核 TLS 模式 |
| 仅 `uspace` | 关闭 | 保持原有用户态模式 |
| `tls,uspace` | 关闭 | 与仅 `uspace` 相同 |

Cargo cfg 不跨包传播，因此依赖边必须转发 feature。`cpu-local/uspace` 转发到 `ax-cpu/uspace`；HAL 和动态平台同时向自身直接依赖转发，`somehal` 继续转发到 `someboot`。

### 1.2 所有权与接口

RISC-V 用户态模式在内核使用 `tp` 保存当前执行上下文，并令 `sscratch` 为零；内核 TLS 模式使用 `tp` 保存 TLS，`sscratch` 保存 CPU 区域。LoongArch 用户态模式同样以 `tp` 保存当前上下文。x86_64 与 AArch64 保留既有独立当前上下文寄存器，双开时也不安装内核 TLS。

`KernelTlsBase::for_task_context` 在无内核 TLS 模式拒绝非零基址。任务上下文布局、事务提交、抢占及迁移顺序不变。内核线程指针接口仅在 `kernel_tls` 下可用；用户态 TLS 仍由用户上下文保存和恢复。原有合法 feature 组合保持行为；要求内核 TLS 的调用方不能在开启 `uspace` 后继续假定其可用。

## 2. 启动与验证

同一映像的 bootstrap、SMP、上下文切换和链接布局必须采用相同模式，否则编译通过仍可能产生寄存器破坏。

### 2.1 资源与链接

`ax-runtime` 双开时采用原有无内核 TLS 的分配、零标识和 `Unsupported` 返回路径，不创建或安装内核 TLS 资源。`someboot::Build::kernel_tls` 保存 build.rs 的同一次计算结果，用于 `render_linker_script`；不能使用随后为 AArch64 启动调整的 `Build::uspace` 反推 TLS 模式。无 TLS 链接布局继续拒绝意外的 `.tdata` 和 `.tbss`。

### 2.2 回归与发布

`scripts/test/check_kernel_tls_modes.py` 编译真实 `cpu-local` 及其 `ax-cpu` 依赖，检查向 CPU 层的 feature 传播和两个底层包的 rustc cfg，并在同一构建目录切换四种组合。QEMU 验证独立保留 ArceOS 多任务 TLS、Starry 用户态 clone/TLS 和 Axvisor 宿主 TLS 路径，不能用宿主配置检查替代。

旧注册表基线仍含互斥报错。先发布能够接受双开的新版本，再验证真实注册表基线并恢复相关 semver 检查。回滚通过撤销模式改动和依赖版本更新完成；已采用双开的调用方需要同时恢复为单独 `uspace`。合入前应由启动与调度领域审查人核对寄存器所有权及真实运行证据。
