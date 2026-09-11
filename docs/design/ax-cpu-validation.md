# ax-cpu 重构验证记录

本记录区分已取得的目标执行证据与仍需完成的验收。CPU 用例位于 `test-suit/arceos/cpu`，板卡入口位于 `test-suit/arceos/board-orangepi-5-plus/pmu`；ax-cpu 本身不包含测试入口或硬件替身。

## 1. 验证基线

当前集成基线为 `481d6bdc4df1302321299678a1e917890ff5942b`。此前执行证据以 `b2c8d4f05bb9cfeab6de89cdff3b4f8552877113` 为基线；新基线只更新测试协议和工具依赖，CPU 实现保持一致。实现提交随 PR 一并固定；本节记录实际完成的验证批次，CI 状态以 PR 当前提交为准。

### 1.1 参考实现

PMU 对照本地 Linux `8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 的 ARM PMUv3 实现。PR #2274 的接口需求固定到 `535c98535bb241e85c7181e85aaa16c3e2d25577`，适配说明见 [perf 迁移](ax-cpu-perf-adaptation.md)。PR 的实现不作为硬件正确性的证明。

### 1.2 静态与系统检查

最终 `cargo xtask test` 完成 58 个软件包。六个主要受影响软件包的 `cargo xtask clippy` 完成 159 项；随后 IRQ 转发与 tag 容量边界清理完成 59 项补查。最新 VMX 与 perf owner 修正的定向检查完成 133 项；随后 somehal 屏障与 PMU affinity 校验完成 66 项补查。CPU crate 未执行或新增宿主测试。

2026-09-10 最终 CPU QEMU 批次：AArch64 9/9、x86_64 9/9、RISC-V 6/6、LoongArch 4/4，共 28 个用例。x86 VMX 使用本机 Intel KVM，SVM 使用 TCG；LoongArch 使用 LVZ 容器中的 QEMU。四架构 `task-tls` 和 `task-scheduler-irq-window` 各通过一次，共 8 个真实运行期用例。CI 规划器 66 项测试及 `check_ci_routing.py` 通过，CPU 组和 PMU 板卡组均已注册。

## 2. PMU 实机证据

`arceos-cpu-pmu` 使用公共 affinity 接口依次固定到每个 CPU，读取本核能力并独占本核 PMU。IRQ 回调通过 `ax_hal::trap::interrupted_context` 的实际值快照验证被打断现场，PPI 来自固件描述。

### 2.1 QEMU

2026-09-10，AArch64 cortex-a53 和 max 四核配置均通过计数、溢出、暂停恢复、每核独立值及真实 IRQ 生命周期用例。每核两次溢出 IRQ，撤销并同步回调后计数保持不变。max 模型报告 PMU version 6，cortex-a53 报告 version 1；两者均不具备 PMICNTR。

### 2.2 OrangePi 5 Plus

2026-09-10 18:08 +08，执行 `cargo xtask arceos test board --board orangepi-5-plus --test-case pmu -b OrangePi-5-Plus`，板卡 `OrangePi-5-Plus-1`、会话 `073ea338-06b2-4af8-a70b-178a01a35517` 完整通过。实际观测的 CPU 身份与 IRQ 结果为：

| CPU | MIDR | part | PMU version | 可编程计数器 | 溢出 IRQ 次数 |
| --- | --- | --- | --- | --- | --- |
| 0–3 | `0x412fd050` | `0xd05` | 4 | 6 | 每核 2 |
| 4–7 | `0x414fd0b0` | `0xd0b` | 4 | 6 | 每核 2 |

这些编号来自本次实际 MIDR 读取，不用于推断其他系统的 cluster 布局。两组 PMCEID 不同，测试按每核探测结果编程；每核停用的独立哨兵计数值在其他核测试及 IRQ 后保持不变。此用例没有使用 TCG 的 cache/branch 数值证明真实性能，也尚未覆盖 EL0 直接读取或 PMICNTR 硬件。

## 3. 确定性回归

回归均通过 ArceOS 入口执行旧行为失败及同一用例修复后成功。原始日志保留在本地 `/tmp/ax-cpu-*.log`，不将日志路径本身视为可独立重现的证明；用例源码随分支交付。

### 3.1 启动与描述符

`paging-el2` 的 boot vector 用例将 guest ESR_EL1 清零后触发 SVC，旧 EL2 分派误读 ESR_EL1 时失败，读取 ESR_EL2 后通过。另一个用例将 someboot 的描述符交给运行期 `El2Pte` 解码；旧 PXN 位使不可执行页变为可执行，改用 EL2 XN 后通过。该用例证明编码契约一致，尚不等同于实际取指权限故障证明。

### 3.2 x86 客户机

VMX/SVM 客户机模式识别、LMSW 源值宽度、EPT 根对齐均取得红绿证据。客户机入口覆盖 VMX launch/resume、SVM 重入、x87/YMM 和 FXSAVE 路径，以及客体修改 CR2/LSTAR 后的宿主恢复。AMX、CET、XSAVES supervisor 状态和 XFD 特定硬件仍未取得执行证据。

### 3.3 客户机广播失效

AArch64 `guest-entry-stage2` 在双核环境中完成真实映射替换：CPU 0 的 EL1 客户机先读旧页并等待，CPU 1 清除描述符、广播失效、安装新页并再次广播，随后客体继续读取。旧 ALLE2IS 稳定返回旧值 `0x123`，客体域 ALLE1IS 返回新值 `0x456`。测试在进入客体前等待更新线程确认已迁到 CPU 1，避免测试自身依赖调度时机。旧实现红日志为 `ax-cpu-stage2-live-red-final.log`。

VMX 的 `synchronize_long_mode` 还覆盖 LME 与 CR0.PG 的四种组合，并检查 EFER.LMA 和 VM-entry IA32e 位同时更新。无条件置 LMA 的旧行为失败，同一客户机用例修复后通过。

## 4. 用户执行边界

`cpu/user-entry` 使用 freestanding ArceOS 镜像，在四核 AArch64 QEMU 中逐核通过真正的运行期用户线程进入 EL0。每个线程的 `TaskAddressSpace` 持有独立页表、代码页和栈页；`UserExecutionContext` 完成调度及地址空间验证后进入 CPU 原语。

### 4.1 读取与撤销

用例向停用的 counter 0 写入 `0x123456`，授予当前用户上下文读权限，并在 EL0 执行 MRS 后以 SVC 返回核验读值。撤销权限后重入同一代码，MRS 必须产生异常，保存 PC 为 `0x10000004`。

### 4.2 用户 IRQ 快照

cycle counter 只计入 EL0 执行，并触发真实溢出 PPI。回调断言 privilege 为 User，PC 位于用户代码页，SP 与 FP 均为用户栈顶；四核均观测到 `0x10000010`。用例关闭来源、同步并释放 IRQ 后才退出任务。记录为 `ax-cpu-user-all-cpus.log`；这不替代 perf 的 mmap 权限所有权策略。

### 4.3 实机与修复表

同一用户态用例在 OrangePi-5-Plus-1 八核通过，板卡会话为 `8effe03c-0d28-47d4-8cd7-7e2b8867035d`。该阶段裸机 bin 的 SHA-256 为 `c3f574de25562e6457028f83f5e0e9e87a587b4f42889139ebfb7670b023d52a`；后续修改后需用新映像重新验证，不能借用旧摘要。

`user-entry/src/fixup.rs` 另外声明地址顺序相反的两条真实缺页恢复记录。原来的启动排序移动 field-relative 表项，访问 null 时找不到恢复位置并恐慌；只读线性查找后，两次实际缺页均恢复到对应代码，四核用户 PMU 用例继续通过。异常表现在没有运行期初始化或共享写入，RISC-V 的 section-relative 格式与其他目标的 field-relative 格式通过选中后端的常量区分。

阶段一 MMU 配置也按本核 `ID_AA64MMFR0_EL1.PARange` 和当前描述符的 48-bit 上限选择 IPS/PS。旧 EL1 固定 48-bit 配置在 Cortex-A53 上被新用例确定性拒绝，调整后同一 `paging` 用例通过。


## 5. 启动与映射生命周期补充

x86 启动 IDT 在低地址映射下创建后不能直接用于已经移除该映射的运行期。ArceOS `paging` 真实 INT3 用例在这种复用下超时重启；改为当前映射构造、调用方持有的 `BootVectorTable` 后同一用例通过，日志 `ax-cpu-x86-boot-int3.log` 与 `ax-cpu-x86-boot-int3-2.log`。CR0、NXE、XSAVE、栈交接及 TSC_ADJUST 读取已集中到 CPU 功能入口，PIT 校准和 PIC 屏蔽仍由 someboot 管理。

LoongArch `guest-entry` 增加相同 guest ID、相同根下的实际数据页替换，验证第二次进入读取新物理页。当前 LVZ 模拟器在原指令下也通过，因此本用例不能独立证明 INVTLB 的 guest-ID 选择域；该域按 Linux KVM 的 `INVTLB_ALLGID` 核对。实体 LVZ 仍是未验证环境。

## 6. Starry 消费者授权回归

`cargo xtask starry test qemu --arch aarch64 -c qemu/system/perf-hw-cycles` 在没有 `config1.rdpmc` 授权的普通事件上 mmap metadata。旧实现无条件发布能力位，真实用例报出 `perf published direct PMU access without event authorization` 并失败；修复后能力位、index、pmc_width 保持零，普通计数 read 继续通过。红绿日志为 `ax-cpu-perf-authorization-red.log` 与 `ax-cpu-perf-authorization-green.log`。该证据用于消费者 ABI，不替代 ArceOS CPU 验证。

### 6.1 普通计数授权

以下表格只评价本次已核验路径，不代表整个系统调用的所有参数组合已经完整兼容。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| `perf_event_open/aarch64-241` | [Linux v7.1 ARM PMUv3](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/perf/arm_pmuv3.c) | 普通计数事件不因创建而取得 EL0 直接读取权限 | perf open → HwPerfEventState / task CountingState → owner CPU | 正确 | 本次仅核对普通 cycles 事件；实际创建及 read 回归通过 |
| `mmap/aarch64-222` | [Linux v7.1 arch_perf_update_userpage](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/perf/arm_pmuv3.c) | 未授权事件的 metadata 不发布 cap_user_rdpmc | device_mmap_rdpmc → RdpmcMapping::install → PerfRdpmcPage | 正确 | 同一 ArceOS CPU 重构下，Starry cycles 用例取得确定性红绿 |
| `read/aarch64-63` | [Linux v7.1 perf core](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/events/core.c) | 未授权直接读取仍可经 fd 读取计数 | perf 文件读取 → HwPerfEventState → owner CPU counter | 正确 | 同一用例 read 返回 8 字节；不以 TCG 数值证明真实性能 |
| `munmap/aarch64-215` | [Linux v7.1 mmap](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/mmap.c) | 撤销 metadata VMA 后由 VMA 引用释放页面 | VMA 所有者 → PerfRdpmcPage Arc；事件只持 Weak | 正确 | 用例 mmap 后实际 munmap 成功，后续计数继续 |
| `ioctl/aarch64-29` | [Linux v7.1 perf core](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/events/core.c) | RESET/ENABLE/DISABLE 不隐含 EL0 授权 | perf ioctl → event enable/disable → owner CPU | 无法确认 | cycles 正常路径通过；此用例未逐个断言 ioctl 返回值，完整错误语义未在本次新增验证 |


### 6.2 任务采样启用

`perf-hw-sample` 暴露任务级事件未设置 PMCR.E：系统事件显式 start，但任务事件仍依赖旧 CPU 初始化隐式启用。原实现真实回归没有生成任何样本，日志 `ax-cpu-final-perf-sample.log`；Starry `hw_owner` 改为本核首次访问时独占 reset，一次初始化后保留所有活动槽，每次配置完成后 start，日志 `ax-cpu-final-perf-sample-green-2.log` 生成实际样本并通过。CPU 不包含该初始化标志，Starry 使用自己拥有的 per-CPU 标量，在 IRQ/抢占保护区间发布。

该回归同时覆盖 `perf_event_open(采样)/241` → task CountingState → owner PMU、`mmap(采样环)/222` → VMA/ring、`ioctl(ENABLE)/29` → 调度启用 → overflow IRQ → ring 可见记录的正常路径。周期上限仍为上层 u32 策略，CPU preload 使用本核实际宽度；溢出状态保留 u64，不截断硬件位 32。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| `perf_event_open(采样)/aarch64-241` | [Linux v7.1 PMUv3](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/perf/arm_pmuv3.c) | 任务采样在拥有的 CPU 上编程并产生溢出 | task CountingState → perf_sched_in → hw_owner::on_pmu | 正确 | perf-hw-sample 无样本红、真实样本绿；仅本次固定周期正常路径 |
| `mmap(采样环)/aarch64-222` | [Linux v7.1 perf core](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/events/core.c) | 映射环包含完整 SAMPLE 与实际 PC | event ring → IRQ sampling registry → VMA 可见 metadata/data | 正确 | 真实用例检查非空记录及非零 PC |
| `ioctl(采样启用)/aarch64-29` | [Linux v7.1 PMUv3](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/perf/arm_pmuv3.c) | per-counter enable 与 PMCR.E 均成立才能计数 | event enable → perf_sched_in → configure/start/preload/enable | 无法确认 | 进入采样后的样本已证明；现有用例未逐个断言 ioctl 返回值，不能宣称完整 ioctl 语义 |

## 7. 最终板卡与下游

最终 PMU 计数/IRQ（含 affinity 校验）会话为 `646bb6ac-888e-4c03-9404-2ff5c5a4ee42`，最终用户访问/IRQ 会话为 `ad6427a9-bc54-4ebf-acff-cf3ccbd60d5e`，两者均在 OrangePi-5-Plus-1 八核通过。用户访问用例新增有效空 TTBR0 后，null 探测在板卡上明确产生翻译故障；原测试使用物理 0 根，在板卡触发外部页表遍历异常，不能把该异常当作普通缺页处理。修复后的同一用例也在 QEMU 通过。

AxVM 实际 smoke 通过 AArch64、RISC-V、LoongArch LVZ、x86 VMX KVM 和 SVM TCG。AArch64 的 Linux 客体 GICv3/ITS timer stress 通过；x86 VMX 的 Linux direct ACPI 客体启动通过。后两者分别验证真正运行中的 Linux 客体，而非只启动宿主 shell。AMD KVM、实体 LVZ、T-Head cache、PMICNTR/PMUv3p9、AMX/CET/XFD/supervisor XSAVE 与 EL3 启动仍未取得相应环境执行证据。

## 8. 性能与映像

同一主机、Intel KVM、单核、256 MiB、`host,-la57`、同一 `arceos-scheduler-latency-bench` 和七轮测量，基线来自独立的 `b2c8d4f05b` 源码副本。结果记录任务切换和无切换 yield，而非从 QEMU 墙钟启动时间推断性能。

| 指标 | dev 基线 | 重构 | 差异 |
| --- | --- | --- | --- |
| 切换中位数 | 312 ns | 305 ns | -2.2% |
| 无切换 yield 中位数 | 105 ns | 108 ns | +2.9% |
| 切换七轮范围 | 307–313 ns | 304–328 ns | 观察到波动，不宣称稳定加速 |
| 裸机 bin | 1,876,016 B | 1,888,304 B | +12,288 B，+0.65% |

ELF section 对比：text 增加 336 B、rodata 增加 1,608 B、sdata 增加 1,856 B、重定位增加 1,560 B、per-CPU 模板增加 680 B；最终 bin 变化还包含 section 对齐。符号表和字符串表不计入裸机 bin。

VMX 使用同一 guest、同一绑定和固定映射循环，每轮 512 次 VMCALL，七轮 TSC tick 平均值取中位数。逐次探测并失效为 168,156 ticks；一次绑定保存能力、每次验证实际 EPTP 并失效为 152,119 ticks；只在未计时的改表阶段显式失效的诊断对照为 136,224 ticks。重新执行完整回归测得 159,475 ticks，说明绝对时间受运行噪声影响。优化保留每次失效，约减少重复硬件探测成本；不以省略失效的实验版本交付，也不把嵌套 KVM 的数值当成裸机硬件承诺。

EPTP 保留位用例在旧路径返回硬件 VmxEntry 错误、未在 CPU 边界拒绝；现逐次验证完整 EPTP，返回 InvalidRoot。红绿日志 `ax-cpu-ept-reserved-red.log` 与 `ax-cpu-ept-reserved-green.log`，同一用例继续验证真实 remap/reentry 和全部控制区回收。


## 9. 未覆盖环境和接口范围

PMU IRQ 固件解析目前支持公共三 cell GIC PPI；ACPI PMU 路由、SPI 型 PMU、四 cell partitioned PPI 未在本次实现或验收。三 cell 公共 PPI 的多 PMU 节点可合并 interrupt-affinity，但必须覆盖所有启用 CPU；仅覆盖子集时返回 Unsupported。遇到不能确定单一公共 PPI 的拓扑返回 Unsupported，不将平台编号或硬编码 PPI 当作能力来源。板卡 A55/A76 能力均逐核读取。

原 `host-test` CPU 替身和 CPU-local 寄存器源码字符串测试已删除。寄存器和实际 IRQ/任务行为由 ArceOS 用例保护，算法所有者的既有标准库测试仍保留。特殊硬件和领域审查需求在 PR 中明确列出；不能把上述未验证环境写成通过。


## 10. 最新 dev 测试协议迁移

PR 创建期间 #2361 合入 dev，ostool 升级到 0.29，结果匹配改为 `shell_check_steps`。30 个新增 CPU/板卡配置已把成功正则放入被动检查步骤，保留原根级失败正则和超时。技能文档保留上游新增的 UEFI 与 xHCI 说明；未从旧分支覆盖构建或依赖清单。变基后的验证另行记录，旧日志不能替代新协议实际执行。

变基到 `481d6bdc4d` 后重新执行：CPU QEMU 四架构仍为 9/9、9/9、6/6、4/4；OrangePi PMU 会话 `b7caa3c8-6549-4433-a044-1df2562f7334`、用户态会话 `c11eb92f-59fa-479a-830a-ec88b2b26fba` 均完成八核验证。Starry cycles/mmap 授权用例通过新协议，CI 规划器 66 项通过。标准库检查发现上游增加 `serial-rx` 后默认选择列表断言未同步，更新明确期望列表后 58 个软件包通过。

首轮 CI 发现新 CPU 用例未声明静态检查目标，AArch64 FP 用例误在 x86 宿主检查。各用例现声明其实际架构；普通 Rust 用例使用共享 musl PIE target，freestanding 用户态用例使用裸机 target。Clippy 对显式 musl 目标复用构建描述并编译 std，避免以 bare target 检查 std 程序。目标选择回归先确认旧命令缺少共享 JSON/标准库编译参数而失败，再验证新命令。

静态检查器的标准库目标回归日志为 `ax-cpu-clippy-std-target-red.log` / `ax-cpu-clippy-std-target-green.log`。Cargo 同时编译 std 内部与应用依赖的同版本 memchr 时，会串接相同包头的诊断；新增解析回归先失败（`ax-cpu-duplicate-report-red.log`），修复只允许完整相同包头后继续逐条校验既有 Rust #134375 诊断，仍拒绝额外 warning。CPU 用例的独立选项 feature 显式依赖 ax-std，保留关闭默认 feature 时的可构建性。

首轮实现提交 `6ff17198a4b6db1bd6f9ec076e994b6b959b2f62` 的 CI run `34484241126` 取得 AMD KVM 证据：job `102897076648` 使用 `-cpu host,-la57,+svm,+npt,+nrip-save -accel kvm`，CPU 为 AMD Ryzen AI 7 350，smoke 与 direct/OVMF Linux 客体均通过；PCI 枚举 job `102897076496` 也通过。因此前文 AMD KVM 缺少证据仅指本地阶段。该轮 NUC job `102897076726` 在等待 axloader ready 时超时，镜像尚未交接，不是已进入重构 CPU 的运行证据。最新提交的 CI 仍以其自身运行结果为准。
