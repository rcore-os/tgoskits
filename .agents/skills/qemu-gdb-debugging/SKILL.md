---
name: qemu-gdb-debugging
description: 使用 QEMU gdbstub 和 GDB/codelldb 调试 TGOSKits、ArceOS、StarryOS 与 Axvisor 内核，定位动态装载地址、映射源码、设置断点和单步执行，并用 Python 自动化采样调用链与慢路径。
---

# QEMU/GDB 内核调试

内核发生 panic、挂起、性能异常，或需要确认某条路径是否符合 Linux RT 语义时使用本技能。目标是把一次可复现运行缩小为“精确提交、精确 ELF、精确 QEMU 参数、精确断点命中”的证据，而不是凭日志猜测。

## 调试前的边界

1. 记录当前提交、目标架构、构建配置、内核 ELF 的绝对路径和 SHA-256。调试时加载与 QEMU 正在运行的同一份 ELF；不要把另一次构建的 DWARF 当作符号来源。
2. 使用项目入口构建和运行（`cargo xtask ...`）。内核需要保留 DWARF；若 perf/debug 镜像启动失败，先用同一 rootfs 和 ESP 做普通 QEMU 验证，不能把调试工具失败误判为内核失败。
3. `-S` 会暂停所有 vCPU，只有需要在早期启动入口断点时才使用；普通运行用 `-s`，启动后再连接 GDB。退出时先 `detach`/`quit`，再以 QEMU 的退出命令结束实例。
4. 只读定位优先。临时断点、Python 输出和 QEMU 参数不得进入产品代码；根因确认后删除临时调试标记。

## 标准流程

完整命令模板见 [`references/qemu-gdb-workflow.md`](references/qemu-gdb-workflow.md)。核心步骤如下：

1. **确认符号和地址模型**：用 `file`、`readelf -hW/-lW/-SW`、`nm -C` 检查 ELF 类型、`PT_LOAD`、可执行段和目标符号。高地址 PIE、物理别名和最终虚拟地址必须分别记录，不能靠截断或掩码推断。
2. **启动 gdbstub**：在保持原有机器、SMP、UEFI、ESP、rootfs 和串口参数的 QEMU 命令末尾加入 `-s`（等价于 `-gdb tcp::1234`）。需要从复位向量停住时加入 `-S -s`。
3. **连接和映射源码**：`gdb -q <ELF>` 后执行 `set pagination off`、`set architecture ...`、`target remote :1234`。源码不在编译目录时用 `directory` 或 `set substitute-path`；用 `info line *ADDR`、`disassemble /m`、`bt`、`info symbol` 交叉确认 DWARF 行号。Rust 宏、泛型和内联函数可能使当前行只是展开点，必须结合调用栈和反汇编判断。
4. **处理动态装载**：若运行时 PC 与链接地址有偏移，先在一个已知符号或可执行 `PT_LOAD` 上测出 `load_bias = runtime_pc - link_pc`，再用 `add-symbol-file` 加载对应节地址。`python3 scripts/locate_kernel.py` 可解析 ELF、计算偏移并生成 GDB 命令；脚本只读 ELF，不修改目标。
5. **布置断点**：优先符号断点（`break 'Rust::path'`）或源码行断点（`break file.rs:line`）；地址已确认且符号被内联时用 `break *0x...`。一次性观察用 `tbreak`；条件断点只引用该帧中确实存在的局部变量。断点动作使用 `commands`/`silent`/`bt`/`continue`，复杂流程写入 `-x` 脚本，避免手工输入丢失。
6. **单步和多核**：`si`/`ni` 用于指令级边界，`step`/`next` 用于源码级边界，`finish` 返回调用者，`until file.rs:line` 跳到同一函数的目标行。`info threads`、`thread apply all bt 12` 观察所有 vCPU；仅在需要隔离一次切换时临时使用 `set scheduler-locking step`，结束后恢复 `off`。
7. **定位慢路径**：在快速门和慢路径入口成对设点，记录 CPU、目标 CPU、任务状态、调度策略、pending/need-resched 标志、锁或 guard token、参数和调用栈。对 futex/调度可从 `sys_futex`、`collect_futex_wakes`、`wake_thread_from_current_cpu`、`activate_waking_thread_locked`、`execute_switch_plan` 逐段设点；对用户返回可从 `handle_syscall`、`prepare_user_return`、`validate_prepared_entry` 设点。将工作分为“Linux 语义必须做”和“当前抽象额外做”，不要因看到 context switch 就把必要成本删掉。
8. **形成证据**：保存 QEMU 命令、GDB 命令、第一次命中时的 PC/线程/CPU/参数/栈、分支决定和源码行。一次命中后 `disable` 或退出，避免重复日志改变时序。比较性能时保持 workload、SMP、时钟源和采样窗口一致。

## 动态地址和 Python

`scripts/locate_kernel.py` 接受运行时 PC 与符号（或链接地址），从 `readelf`/`nm` 读取链接地址，计算 `load_bias`，并输出 `add-symbol-file`、`break *ADDR` 等可粘贴命令。始终使用 `python3`，例如：

```bash
python3 .agents/skills/qemu-gdb-debugging/scripts/locate_kernel.py \
  --elf target/x86_64-unknown-none/release/starryos \
  --symbol 'starry_kernel::syscall::time::sys_clock_gettime' \
  --runtime-pc 0xffffffff801ea530
```

脚本无法证明重定位正确性：运行时 PC 必须来自当前 GDB 停点，且符号必须是同一构建的非 stripped ELF。若找不到符号，先检查是否加载了错误架构、剥离版 ELF、分离 DWARF，或该函数已内联/被 LLVM 改名。

## 常见失败判断

- `Function "..." not defined` 或 pending breakpoint：符号尚未加载或函数被内联；先用 `nm -C`/`info functions`，必要时按运行时节地址 `add-symbol-file`。
- 低地址与高地址不一致：可能是物理映射别名和内核最终虚拟地址，不要直接给地址加减固定常量。
- `-S` 后没有输出：这是预期的 vCPU 暂停；连接后 `continue`。
- 调试镜像在进入 shell 前 QEMU `SIGSEGV`：先重跑不带 perf/debug 插件的普通 QEMU；若普通路径正常，归类为调试工具或映像问题。
- 断点始终不命中：确认串口提示符、实际 workload 命令、目标架构/SMP 和断点函数是否走了另一实现。

## 完成标准

调试记录能够由另一人用同一提交和命令复现；每个结论都包含源码位置、运行时地址如何得到、断点命中上下文和 Linux 语义对照。只分析不改代码时，不提交临时断点或生成物；需要修复时，再按项目规则增加确定性回归、实现、验证并单独提交。
