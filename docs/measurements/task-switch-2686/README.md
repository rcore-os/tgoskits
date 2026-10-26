# RK3588 任务切换开销修复证据

## 1. 实验边界

本文记录 [#2686](https://github.com/rcore-os/tgoskits/issues/2686) 的实板归因与验收。测量契约见 [RK3588 客户机任务切换测量契约](../../../design/task-switch-measurement.md)。`AXTEST_SUITE_OK` 表示完整有效测量，性能目标另按 `TASK_SWITCH_SUMMARY.avg_cycles < 800` 判断；平均值不代表最坏时延保证。

### 1.1 源码与硬件

旧代码固定为 `bff62c9bce566979a8b940eba0c2c572ec6d3667`，修复使用同一基线。所有表内运行使用同一物理板 `OrangePi-5-Plus-robot-1`，设备树型号为 `RK3588 OPi 5 Plus`，客户机唯一 vCPU 固定在 `cpu@400`，该节点为 `arm,cortex-a76`。板卡服务中的这个名称不同于配置原来的 `BallPick-Robot`，运行时显式覆盖；无法据议题正文证明它就是历史报告使用的同一块板，但本次原配置已经复现历史的 2060 周期结果。

工具链为仓库固定的 `nightly-2026-09-04`。客户机实际使用 musl 身份的 Rust 标准库 PIE 目标 `scripts/targets/std/pie/aarch64-unknown-linux-musl.json`，不是 mini-std 别名。宿主与客户机保持 SMP 能力，客户机容量为 1，客户机没有启用 FP/SIMD 或 qperf。宿主 vCPU 任务与客户机两个测量任务分别保持 FIFO(80)，没有修改调度算法、运行队列所有权、中断处理或 FPU 策略。

### 1.2 工作量与产物

每次正式运行都保留 1 亿次读计数器、50 万次线程创建/回收，以及 30 轮、每轮 1000 万次任务切换。`main.rs`、`bencher.rs` 和 `cycle.rs` 与旧代码逐字节一致；每个计时区间仍包含两次切换、原子交接与 peer 的 GPIO3_C6 低电平写入，主任务的高电平写入仍在计时终点之后。不扣除仪器成本、不删 GPIO、不缩小工作量，也没有新增成功重试。

正式日志的 CSV 保存全部 30 轮，TXT 保存从客户机 `Bencher start` 到 `AXTEST_SUITE_OK` 的完整测量段，仅去掉 ANSI 控制序列及重复空行；JSON 保存汇总与原始完整日志的 SHA-256。原始日志还包含构建、部署和串口启动过程，摘要用于核对来源。客户机 BIN 身份为：

| 镜像 | SHA-256 |
| --- | --- |
| 旧代码、原通用配置 | `7f94090b3abbf489d2fbeae4b4a06daace84ed81f2a73d51c58f730af3806406` |
| 旧代码、A76 配置 | `05b59df1b5d86f018ca9605a90b46d9352918ecaf1cf33b363449ae72155173b` |
| 修复代码、A76 配置两次运行 | `564dfd8a8bfd1eb8df96391882095afc1e2efad73f56ee83b0aadbc24694c00e` |

第一次修复运行曾显式重复传递 `+strict-align`。清理此冗余参数后重新构建，BIN 逐字节相同，`.text` 摘要同为 `5451f22cc59da044dd9036ed4ecb992fa26d906530e4cb8a2d8e9fce6ad7d6e3`；严格对齐继续来自共享目标 JSON，正式配置不保留命令行不稳定特性警告。第二次运行直接使用最终仓库配置，客户机 BIN 与第一次相同。

## 2. 根因与修复

普通客户机 `yield` 不主动请求 VM exit，但它经过的完整运行路径不只是调度器源代码中的队列操作。以下结论来自实际 ELF、同板单变量诊断和完整工作量对照，而不是根据函数名推测周期收益。

### 2.1 通用内存复制

真实调用链为 `std::thread::yield_now()` → `ax_std::os::libc_compat::sched_yield()` → `ax_posix_api::sys_sched_yield()` → `ax_task::runtime::switch::dispatch::yield_current_cpu()`。调度事务及切换计划的若干 40–80 字节 Rust 聚合值转移被编译为 `memcpy`；旧版兼容层导出的该符号只有 `ldrb/strb` 逐字节循环。因此即使源码没有显式 clone 或分配，实际机器码仍反复支付通用复制成本。

修复留在 `ax_std::os::libc_compat::memcpy()`：相同对齐余数的正常内存先用字节前缀对齐，再以完整机器字复制，最后处理字节尾；其他对齐关系保留合法字节访问。原 ABI、返回指针、非重叠语义、零长度和访问范围不变。使用 `MaybeUninit` 传递原始存储，允许结构体 padding 未初始化；不创建普通引用、不越界、不使用 SIMD 或未对齐宽访问，也不把实现降为可能递归调用同名 libc 符号的复制 intrinsic。

Linux 对照固定为 v7.1、`8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 的 `lib/string.c` 与 `arch/arm64/lib/memcpy.S`。采用的是正常内存批量复制原则，不照搬依赖未对齐访问的汇编，更没有自创调度算法。

### 2.2 GPIO 与代码生成

旧 `set_gpio_bit()` 为改变一个引脚读取两个半区，再全掩码写回两个半区。实板 `GPIO_VER_ID=0x0101157c` 对应 Linux `gpio-rockchip.c` 的 V2_1；其硬件支持单 bit write-mask。修复只写目标半区与目标位，不再读改写其他引脚，显式整 bank 清零仍保持原语义。诊断中的单次 GPIO low 操作从约 485 周期变为约 36 周期，二者都包含约 31 周期的 PMU 首尾读取；这些数值不从正式结果中扣除。

客户机既然固定在 A76，就使用该 CPU 的代码生成能力与代价模型，并复用裸机 AArch64 已采用的短复制内联代价策略。最终 flags 为 `-Zstack-protector=strong -Ctarget-cpu=cortex-a76 -Ctarget-feature=-neon -Cllvm-args=--max-store-memcpy=16`。严格对齐、soft-float ABI 和禁用 SIMD 的约束保持；仅客户机使用这些参数，宿主仍使用原通用配置。该镜像不能迁移到 A55 或任意 ARMv8-A CPU，配置没有改变全局目标或 Rust 原子内存序。

### 2.3 宿主与客户机分离

短诊断使用相同的修复客户机镜像，在新宿主上带 GPIO 的三轮均值为 794、794、793 周期，在旧宿主上为 795、794、794 周期。差异远小于从 2060 周期下降的幅度，支持主要成本位于客户机执行链，而非宿主调度策略；这不是“绝无异步 VM exit”或“宿主没有任何成本”的证明。

全部短诊断记录在 [diagnostics.csv](diagnostics.csv)、[diagnostics.txt](diagnostics.txt) 和 [diagnostics.json](diagnostics.json)。它们使用每窗口 20 万次切换及独立诊断标记，不能代替正式 3 亿次验收。诊断结束没有正式汇总，未改动的 ktest 因客户机停止而判失败；文件明确记录这种状态，未把诊断结果包装成套件通过。qperf 明显增加热路径成本，只用于阶段定位；LTO 与单独调整 codegen units 的对照没有改善，因此未纳入最终配置。

## 3. 完整工作量结果

本节四组数据来自 PR 首版提交 `4af1cff1eb84615408c895728ec2c339113351bc` 及其基线对照。当时 A76 模式以 `[env] RUSTFLAGS` 提供完整参数；审查确认它会遮蔽 axbuild 的可选调试参数，因此后续配置迁移为根级 `BuildInfo.rustflags` 追加项。历史数值及日志不改写为新运行；第 5 节记录迁移后的独立构建与产物身份核对。

原配置、旧代码新配置和修复版均使用相同工作量、物理板、CPU 绑定与计数方法。配置与代码收益分别列出，不用不同条件的最好轮次拼接结论。

### 3.1 正式对比

每行包含一次完整、成功结束的 30 轮测量。最小与最大周期是程序原有的往返周期除以二后的单次切换估计，不是独立单向上下文切换的最坏执行时间证明。

| 版本与模式 | 平均周期 | 各轮平均周期范围 | 平均 ns | 最小周期 | 最大周期 | 数据 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 旧代码、原配置 | 2060 | 2059–2063 | 2748 | 1967 | 9738 | [CSV](baseline.csv)、[日志](baseline.txt) |
| 旧代码、A76 配置 | 930 | 929–931 | 1335 | 888 | 5781 | [CSV](baseline-a76.csv)、[日志](baseline-a76.txt) |
| 修复代码、A76 配置第一次 | 794 | 794–795 | 1018 | 754 | 6046 | [CSV](fixed-1.csv)、[日志](fixed-1.txt) |
| 修复代码、A76 配置第二次 | 794 | 794–795 | 1018 | 755 | 5817 | [CSV](fixed-2.csv)、[日志](fixed-2.txt) |

两次修复运行的汇总平均值均为 794 周期，相对原配置降低约 61.5%，相对旧代码相同 A76 配置降低约 14.6%。两个旧版本均不满足平均小于 800 周期，两次修复运行均满足；每次都完成 3 亿次切换，全部 60 轮的单轮平均值也都小于 800。四次正式运行均输出 `AXTEST_SUMMARY pass=1 fail=0 skip=0 total=1` 和 `AXTEST_SUITE_OK`，最外层命令退出码均为 0。

### 3.2 判定与限制

同一结果检查器核验 30 个唯一连续轮次、每方向 500 万次、聚合 1000 万次/轮、非零统计及套件最终标记，然后按 `<800` 判断。旧原配置得到 `2060 >= 800`，旧代码 A76 模式得到 `930 >= 800`，修复两次均为 794。功能边界测试旧、新实现均能通过，不把这些功能测试伪称为性能红测。

定时器频率为 24 MHz。日志推导的 CPU 频率随区间和版本不同，原配置切换阶段约 0.750 GHz，旧代码 A76 约 0.697 GHz，修复两次均约 0.780 GHz；没有修改调频逻辑或按频率缩放周期结果，也不宣称锁定了完全相同的硬件频率。PMU 与系统计数器的首尾读取区间本身不同，短区间固定观测成本也会影响频率比值。正式目标使用原始 PMU 周期统计，纳秒与推导频率作为原有附加观测保留。

最大值仍显著高于 800 周期。本次没有示波器设备，不声称已实测 GPIO 波形；保留真实 MMIO 与原计时位置，并使用硬件版本和 Linux 寄存器契约核对写入语义。一次正式尝试被会话中断，只到预热阶段，未纳入成功数据；所有表内数据都来自完整终态。

## 4. 复现与正确性验证

正式入口使用仓库 `cargo xtask`，以下板型覆盖对应本次可用服务，不要求修改仓库默认服务或固件。

### 4.1 实板命令

在包含本修复的仓库执行，`BOARD_SERVER` 按实际板卡服务设置。若使用其他板型，必须确认同样的 RK3588 GPIO 资源和 A76 CPU 绑定，不能按名称猜测兼容。

```bash
cargo xtask ktest board -p axvm --test task_switch -b orangepi-5-plus \
  --config apps/axvisor/normal/board-ballpick-robot/task-switch/build-aarch64-unknown-none-softfloat.toml \
  --board-config apps/axvisor/normal/board-ballpick-robot/task-switch/board-ballpick-robot-task-switch.toml \
  --board-type OrangePi-5-Plus-robot --server "$BOARD_SERVER"
```

旧代码对照在独立、未修改源码的基线 worktree 中执行同一 ktest；原配置直接采用该提交配置，新配置仅在实验 sidecar 中加入上述客户机 flags。原工作树不来回回滚生产代码。最终提交不包含实验 sidecar、RUSTC wrapper 或诊断 feature。

### 4.2 回归记录

`memtest` 从真实导出函数入口覆盖零长度、对齐余数、机器字边界、跨字长度、返回地址、源不变、目标 canary 和带未初始化 padding 的对象；检查不读取 padding。实际 AArch64 ELF 含对齐 `ldr/str` 宽循环，无递归调用或 SIMD，测试通过黑盒函数指针真实调用该符号。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt`、`git diff --check` | 通过 |
| `cargo xtask clippy --package ax-std` | 38/38 通过 |
| `cargo xtask clippy --package arceos-test-suit` | 49/49 通过 |
| `cargo xtask clippy --package task-switch` | 清除诊断后原有 3/3 组合通过 |
| `cargo xtask arceos test qemu --arch aarch64 --test-group rust --test-case memtest` | 增强测试在旧、新实现上均通过 |
| 同一 `memtest`，`--arch x86_64`、`riscv64`、`loongarch64` | 各 1/1 通过 |
| AArch64 `--test-case task-yield`、`task-rt-policy` | 各 1/1 通过 |

源码、GPIO、配置和诊断清理分别完成了独立审查；测试通过不能代替 `MaybeUninit`、对齐、范围和指针来源的 unsafe 证明。最终跨任务验收同时核对了设计、真实产物和板卡数据。

## 5. 构建参数审查返修

PR 的 [参数来源审查](https://github.com/rcore-os/tgoskits/pull/2690#discussion_r4376118474) 指出 `[env] RUSTFLAGS` 会覆盖 axbuild 生成的 target rustflags。根级 `BuildInfo.rustflags` 通过既有 `append_cargo_rustflags()` 追加客户机参数，让工具链继续拥有栈保护与调试参数；完整接口见 [构建参数追加契约](../../../design/axbuild-rustflags.md)。此次返修不修改客户机源码、GPIO、调度器或计时边界。

### 5.1 调试能力红绿证明

旧、新配置都通过 `cargo xtask arceos build --config <实验配置>` 构建真实 `task-switch` 客户机。实验配置分别由对应版本的正式 guest-build 配置增加 `[env] DWARF = "y"` 或 `BACKTRACE = "y"` 得到。编译 wrapper 仅记录并原样传递 rustc argv；产物通过 `readelf -SW <客户机ELF>` 检查，不能只用生成配置文件中的参数证明生效。去除本机路径与编译元数据后的参数及产物摘要保存在 [rustflags-review.json](rustflags-review.json)。

| 配置 | 构建退出码 | 实际调试参数 | ELF `.debug_info` | 验收 |
| --- | ---: | --- | --- | --- |
| 旧配置加 `DWARF=y` | 0 | 无 debuginfo、strip=none 或 frame-pointers | 无 | 同一检查器退出 1，符合预期失败 |
| 新配置加 `DWARF=y` | 0 | `-Cdebuginfo=2 -Cstrip=none -Cforce-frame-pointers=yes` | 有 | 检查器退出 0 |
| 新配置加 `BACKTRACE=y` | 0 | 仅增加 `-Cforce-frame-pointers=yes`，不要求 DWARF | 不作为此模式的要求 | 检查器退出 0 |
| 新配置不启用调试 | 0 | 保留栈保护、A76、禁用 NEON、短复制参数 | 不作为此模式的要求 | 检查器退出 0 |

旧配置的失败是构建成功但所需参数被遮蔽，并非构建器报错。`BACKTRACE` 只启用帧指针，与 `DWARF` 的语义不同；新配置没有通过为所有模式强制开启调试来绕过问题。三个绿色构建均核验自动栈保护和客户机专属参数共存。

返修实现通过 `cargo fmt`、`cargo xtask clippy --package axbuild`（1/1）、独立局部审查及最终差异检查。提交后使用 `cargo xtask test --since 4af1cff1eb84615408c895728ec2c339113351bc`，实际选择 axbuild，485 个测试全部通过，包括新增的 std/no_std builder 默认参数共存与显式环境来源测试；没有用 Clippy 代替单元测试执行。

### 5.2 性能证据复用

完成调试构建后，再直接使用仓库的正式 guest-build 配置、不带编译 wrapper 构建客户机。`cmp` 确认新 BIN 与原第二次正式运行的 BIN 逐字节相同；ELF 与 `.text` 的 SHA-256 也分别相同，因此复用原两次完整 794 周期测量，不将本轮构建检查冒称为新的板卡运行。

| 产物 | 返修前后相同的 SHA-256 |
| --- | --- |
| 客户机 BIN | `564dfd8a8bfd1eb8df96391882095afc1e2efad73f56ee83b0aadbc24694c00e` |
| 客户机 ELF | `303ab35d1d20f988dec56ce1b49eb615670b5748244d9447edb3d238816bad98` |
| ELF `.text` | `5451f22cc59da044dd9036ed4ecb992fa26d906530e4cb8a2d8e9fce6ad7d6e3` |

DWARF 与 BACKTRACE 构建的 BIN 不同，只用于调试功能证明，不套用正式性能结论。相同正式 ELF 的 [memcpy 反汇编](memcpy-a76.txt) 记录了 `llvm-objdump -d --disassemble-symbols=memcpy task-switch.elf` 输出和镜像摘要：对齐路径采用 `ldr/str`，字节路径采用 `ldrb/strb`，没有递归调用或 SIMD。该证据限定于记录的工具链与镜像，编译器升级后仍需重新核对。
