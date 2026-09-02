# QEMU/GDB 调试命令手册

## 1. 构建和核验 ELF

```bash
git rev-parse HEAD
sha256sum target/x86_64-unknown-none/release/starryos
file target/x86_64-unknown-none/release/starryos
readelf -hW target/x86_64-unknown-none/release/starryos
readelf -lW target/x86_64-unknown-none/release/starryos
readelf -SW target/x86_64-unknown-none/release/starryos
nm -C --defined-only target/x86_64-unknown-none/release/starryos | rg 'sys_futex|wake_thread|execute_switch_plan'
```

ELF 是 PIE/DYN 时，`PT_LOAD` 的 `VirtAddr` 是链接视图；运行时仍要用 GDB 停点确认 load bias。高地址内核不能因为 QEMU 物理地址较低而改用物理别名断点。

## 2. 启动 QEMU

保留项目生成的全部参数，只增加 gdbstub：

```bash
# 运行到串口提示符后再调试
<cargo xtask 生成的 qemu 命令> -s

# 从复位入口暂停，适合早期启动/重定位
<同一命令> -S -s
```

若 1234 已占用，改为 `-gdb tcp::2345` 并在 GDB 中连接 `:2345`。调试多核切换时不要改变 `-smp`、机器类型或时钟参数。

## 3. 连接、源码和断点

```gdb
set pagination off
set confirm off
set disassemble-next-line on
set architecture i386:x86-64
target remote :1234
directory /home/zhourui/.codex/worktrees/8049/tgoskits-dev
set substitute-path /build/tgoskits /home/zhourui/.codex/worktrees/8049/tgoskits-dev
info files
info threads
break 'starry_kernel::syscall::time::sys_clock_gettime'
break os/StarryOS/kernel/src/syscall/sync/futex.rs:373
break *0xffffffff800cffd0
condition 3 cpu_id == 1
commands 3
  silent
  printf "hit pc=%p cpu=%d\n", $pc, cpu_id
  bt 12
  info args
  continue
end
continue
```

Rust 名称含特殊字符时用单引号；函数被内联或只有局部 LLVM 符号时，使用 `nm -C` 输出的地址配合 `break *ADDR`。`condition` 中的局部变量只有在当前栈帧和优化信息可用时才可靠。

## 4. 动态重定位

在 GDB 中先停在一个已知运行时 PC，例如：

```gdb
info symbol $pc
info line *$pc
```

若 `info symbol` 显示的链接符号地址为 `0xffffffff801ea530`，而停点 PC 为 `0xffffffffa01ea530`，则 `load_bias = 0x20000000`。可以按链接地址整体加载：

```gdb
add-symbol-file /abs/path/starryos 0xffffffffa0000000
```

更稳妥的方式是让脚本根据 `PT_LOAD` 和节表生成命令：

```bash
python3 .agents/skills/qemu-gdb-debugging/scripts/locate_kernel.py \
  --elf /abs/path/starryos \
  --symbol 'starry_kernel::syscall::time::sys_clock_gettime' \
  --runtime-pc 0xffffffffa01ea530 \
  --gdb-script-out /tmp/starry-symbols.gdb
gdb -q /abs/path/starryos -x /tmp/starry-symbols.gdb
```

脚本会优先用 `.text`/`.head.text` 节地址；如果 ELF 没有这些节，则使用第一个可执行 `PT_LOAD`。若当前 ELF 本来就是最终高地址 ET_EXEC/DYN 且无运行时偏移，不要重复 `add-symbol-file`。

## 5. 单步和一次性采样

```gdb
thread 4
set scheduler-locking step
si                         # 一条机器指令
ni                         # 越过当前调用
step                       # 进入源码调用
next                       # 越过源码调用
until os/StarryOS/kernel/src/task/futex.rs:1056
finish
set scheduler-locking off
thread apply all bt 12
```

一次性采样可避免长时间停顿改变调度时序：

```bash
gdb -q /abs/path/starryos \
  -ex 'set pagination off' \
  -ex 'target remote :1234' \
  -ex "tbreak starry_kernel::task::futex::ResolvedFutex::wake" \
  -ex 'continue' -ex 'bt 16' -ex 'info args' \
  -ex 'detach' -ex 'quit'
```

如果需要继续运行而只记录一次命中，把 `continue` 放进 `commands`，命中后执行 `disable N` 再继续。

## 6. 慢路径对照记录

建议按以下顺序设置断点，并在每个点记录 `cpu`、目标 CPU、任务状态、策略、锁/guard token 和 `$pc`：

```gdb
break starry_kernel::syscall::sync::futex::sys_futex
break starry_kernel::task::futex::collect_futex_wakes
break 'ax_task::system::task_system::TaskSystem::wake_thread_from_current_cpu'
break 'ax_task::system::task_system::TaskSystem::activate_waking_thread_locked'
break ax_task::facade::scheduling::execute_switch_plan
break ax_runtime::guard::prepare_user_return
break ax_runtime::guard::validate_prepared_entry
```

futex 唤醒中，bucket/domain 查找、阻塞状态转换、就绪队列入队和真正 context switch 通常是语义必需的；重复的 `Arc` 克隆、调度实体快照、全量 guard 校验、无必要的远端通知或统计发布才是优先审计对象。用户返回中，need-resched/信号/地址空间代数检查是必需的；每次 syscall 都重新做不变的上下文验证可能形成通用固定成本。

## 7. 结束会话

```gdb
detach
quit
```

随后通过 QEMU monitor 的 `Ctrl-a x` 或项目脚本正常退出，并确认没有遗留 `qemu-system-*`/`gdb` 进程。不要用杀掉整个工作区进程的宽泛命令。
