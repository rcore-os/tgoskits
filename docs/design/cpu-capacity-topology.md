# CPU 启动容量与拓扑

## 1. 固件语义

容量是 CPU 拓扑的一项只读启动属性，供后续容量感知放置使用。原 PR #2001 在 `ax-hal::dtb` 内重新枚举 CPU、保存固定长度数组，并按 CPU 型号猜测权重；这与当前 someboot 的启动 CPU 优先编号以及 ACPI/FDT 来源选择不一致。

### 1.1 Linux 对照

本地 Linux 源码版本为 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`（Linux 7.1）。语义依据是 `drivers/base/arch_topology.c` 的 `topology_parse_cpu_capacity()` 和 `topology_normalize_cpu_scale()`：按硬件身份关联 CPU 节点，任何参与归一化的 CPU 缺少容量属性时丢弃整组信息，默认全部为 1024；完整信息按最大原始容量归一化。

`CpuIdIter` 在创建时选定 ACPI、FDT 或默认来源，并在容量解析和元数据写入之间保持该选择。仅选中 FDT 时，`capacity_fdt()` 才读取容量设备树；ACPI 路径尚未实现容量发现，统一使用 1024，即使同时存在具有相同数字 CPU ID 的 FDT，也不能读取其中的容量。

`someboot::fdt::CpuCapacities` 复用现有 `cpu_nodes_from_fdt()` 的节点筛选，以 `reg` 匹配启动层实际选择的硬件 ID，不另建 CPU 编号。只归一化启动层选中的 CPU；禁用节点、缺少 `reg` 的节点和 `/cpus` 外的节点不能改变编号或最大值。重复硬件身份无法可靠关联，整组采用默认容量，这是本项目额外的输入校验。

### 1.2 频率与边界

Linux 的完整容量计算还乘以 CPU 参考频率。someboot 当前没有统一 CPU 时钟提供者，因此本实现采用 Linux 在 `of_clk_get()` 不可用时的等频假设，不把 CPU 节点的 `clock-frequency` 或某个型号名称当作已解析的时钟提供者。运行期 cpufreq、热压力、当前负载及 ACPI CPPC 不在本次范围内；本接口不承诺实际峰值吞吐量。

归一化采用 `u64(raw) * 1024 / max(1, max_raw)`。保留 Linux 整数截断：极小值和显式零可以得到 0，全部为零时结果也全部为零。缺失属性与值为零有不同语义。后续消费者必须处理零，不能无条件把容量用作除数。

## 2. 所有权与接口

固件解析、CPU 编号与容量发布共同归 someboot 所有。复用现有 CPU 元数据能够避免另一份运行期表及初始化状态，也避免把固件知识放进操作系统适配层。新增共享软件包或运行期惰性缓存都不能减少这里的所有权复杂度，故不采用。

### 2.1 初始化与读取

`initialize_runtime_metadata()` 在最终高地址初始化期间创建一次 `CpuIdIter`，使用其同源克隆校验整个启动 CPU 集合，再将归一化容量写入同一 `PerCpuMeta` 的末尾。容量随已有缓存维护和 `CPU_AREA_RUNTIME_COUNT` 的 Release 发布一起生效。读取经现有 Acquire 和索引检查，只复制元数据，不访问固件、不分配、不加锁。属性写入完成后不再更新，生命周期与 CPU 元数据相同。

`PerCpuMeta` 新字段位于末尾，既有跳板字段偏移不变；布局分配和缓存维护使用 `size_of::<PerCpuMeta>()`。不修改处理器启动传输、握手状态、寄存器或新的指针所有权。发布前的容量查询返回 `None`，不会把过早查询的默认结果永久缓存。

### 2.2 消费与迁移

平台调用链为 `ax_hal::topology::cpu_capacity(cpu_index)` → `ax_plat::cpu::CpuTopologyIf` → `axplat-dyn` → `somehal::smp` 重导出的 `someboot::smp::cpu_capacity()`。有效逻辑索引返回 `Some(0..=1024)`，未发布或越界返回 `None`；有效 CPU 的固件容量不可用时返回 `Some(1024)`。无 SMP 的消费者只查询运行期允许的 CPU，平台不按 ax-hal 的编译期数组长度裁剪归一化集合。

删除尚未合入的 `ax_hal::dtb::cpu_capacities()` 及专属测试依赖。当前 dev 没有该接口消费者；开放 PR #2064 仍使用旧数组，重放时应逐逻辑索引查询拓扑，并明确处理 `None` 和零容量，同时按当前 `components/ax-task` 架构调整调度接入。本改动不复制该 PR 的调度实现，也不修改其他开放分支。

## 3. 验证与交付

验证分别覆盖固件解析规则与真实平台装配；不以宿主测试代替跨核可见性。该变更涉及启动元数据和共享平台接口，合入前仍需平台维护者审阅本设计及实现；本次交付为待审查 PR。

### 3.1 规则与回归

原 ax-hal fixture 的无容量属性场景在旧实现上得到 `[1024, 530]`，以统一默认 `[1024, 1024]` 判定时失败。重构后将原节点过滤和归一化的独特信号迁入 someboot 通用单元测试，验证非连续硬件 ID、启动 CPU 重排、选中集合、禁用及游离节点、缺失或畸形属性、整数截断和整组回退。

通过 `cargo xtask cross-test --arch aarch64 --package someboot --lib fdt::capacity::tests` 执行实际解析代码，并通过 `smp::cpu_iter::tests` 验证来源优先级与启动 CPU 顺序，用于本地定向规则验证，不新增独立 CI 项。当前 dev 的 someboot x86 库测试仍引用已删除的定时器对象，因此本次不将 someboot 加入宿主标准库测试允许列表，也不改动无关定时器代码。

### 3.2 装配与回滚

ArceOS 综合测试套件新增 `cpu-capacity` 测试项，并纳入 `all`、`SELECTED_TESTS` 和任务工具发现列表，复用现有 Rust 套件 CI。测试在每个实际处理器上的亲和线程中经 HAL 查询所有在线 CPU 的容量，验证无容量属性的 QEMU 固件得到 1024，并拒绝越界索引。该路径同时经过元数据发布、动态平台接口和跨核读取；成功标记及失败规则沿用套件契约，`task-smp-online` 保留原有 SMP/IPI 职责。

按用户要求不在本地运行 clippy 或完整 QEMU 矩阵。执行结果记录在 PR 正文，未执行项不能视作通过。运行期查询复杂度不变为常数；固件匹配仅在启动初始化中执行，时间上界随选中 CPU 数和设备树 CPU 节点数的乘积增长，使用常量额外存储。没有性能收益声明。回滚可整体撤销容量接口、元数据字段及测试接入，无持久状态迁移。
