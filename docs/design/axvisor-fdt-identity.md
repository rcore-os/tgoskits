# Axvisor 宿主与客户机设备树身份

本文定义宿主资源与客户机固件节点的对应规则。实现位于 `virtualization/axvm/src/boot/fdt/core/`，资源归属仍由既有设备图和 VM 配置决定；phandle 转换不新增资源分配权限。本次修改需要在合入前接受虚拟化领域设计审查。

## 1. 节点对应

DTSpec v0.3 的 [phandle 定义](https://devicetree-specification.readthedocs.io/en/stable/devicetree-basics.html#phandle)规定编号在单棵设备树内唯一。宿主和客户机可以重复使用相同数字，但不能据此认定两个节点是同一设备。

### 1.1 机器设备替换

`interrupt::phandle::controller_node()` 根据当前单一机器控制器角色选择 GIC 或 PLIC，多个候选直接报错。`phandle::install()` 保留所选客户机节点的编号，宿主 profile 的编号只作为缺少客户机编号时的分配提示。新设备的中断引用由 `fdt_interrupt_binding()` 从最终客户机树取得，运行时控制器 ID 仍由设备图校验。

ITS 允许多实例。`its::install_registers()` 先按 profile 路径绑定；路径不同时，只接受唯一的相同寄存器区间，不按遍历顺序或相同 compatible 猜测。路径已被另一类设备占用、重复绑定或客户机 ITS 缺少对应实例均拒绝。`timer::install_machine_timer()` 通过唯一的 Arm architectural timer 角色保留客户机路径与 phandle，再更新宿主提供的定时器资源。串口继续使用已有 console identity 和 `replacement_phandle()`。

### 1.2 跨树导入

`FdtTree::copy_subtree_from()` 调用 `import::copy_subtree()`，在目标树副本中先创建全部节点，建立来源 `NodeId` 到客户机 `NodeId` 的对应表，再在来源树解析引用并写入目标编号。修改只作用于导入属性，不扫描替换客户机原有的同值整数；源树保持不变。任一步失败都不提交目标副本。

`reference_offsets()` 按属性绑定跳过 clock、IOMMU、GPIO 等参数，处理 `interrupts-extended`、`interrupt-map`、`msi-map` 和 `iommu-map` 的引用位置。MSI 映射格式依据 [Linux v6.12 PCI MSI binding](https://raw.githubusercontent.com/torvalds/linux/v6.12/Documentation/devicetree/bindings/pci/pci-msi.txt)。子树外依赖和未知属性绑定会拒绝导入；调用者必须扩展真实绑定语义，不能通过相同编号、相同 compatible 或自动导入硬件来绕过错误。已有客户机属性不受导入属性白名单限制。

## 2. CPU 与兼容边界

显式客户机 DTB 的 CPU 必须与实际 vCPU 执行身份一致，同时不能把宿主供电和频率控制能力隐式交给客户机。`cpu::project_cpus()` 将这一策略与通用引用转换分开。

### 2.1 CPU 投影

未配置显式 `phys_cpu_sets` 时，按 `phys_cpu_ids` 选择宿主 CPU，并只投影执行所需的标量属性；配置显式 affinity 时，`phys_cpu_ids` 只表示客户机 CPU 身份，宿主选择由最终 mask 决定。已有客户机 CPU 优先按相同硬件地址对应；宿主选中集合与客户机均为单 CPU 时允许唯一角色对应，并保留客户机 phandle。其他无法确定的对应关系直接报错。投影不复制宿主 OPP、供电、idle-state、cache provider 引用，也不保留整机 `cpu-map`，避免描述未分配的硬件与拓扑。

这一限制作用于显式 DTB 的 CPU 替换路径。宿主完整派生设备树的 `create_guest_fdt()` 也会删除选中 CPU 上的 OPP、时钟、供电、PVTM、NVMEM 和 thermal cooling 绑定；Axvisor 继续转发 SCMI mailbox，以便客户机使用分配设备所需的时钟和供电服务。由于客户机 CPU 节点不再引用宿主 CPU 的 SCMI clock、OPP 和 regulator，标准 cpufreq 驱动没有宿主 CPU 调频入口，不会成为 CPU 电源轨的第二个所有者。本次不新增独立客户机直通设备的任意路径或 GPA 重定位配置；既有直通资源映射仍需遵循已有配置契约，不能把本次身份修复解释为该能力已经可用。

### 2.2 失败与验证

`phandle_index()` 拒绝重复、保留值、错误长度以及 `phandle` 与 `linux,phandle` 不一致的输入。最终运行期固件输出再次检查编号唯一性。它不是任意厂商 binding 的全树语义校验器；未改写的客户机属性仍由对应设备模型和客户机驱动的契约约束。

回归测试在进程内使用真实 `fdt-edit` 解析、编码和 AxVM 合成逻辑，证明编号冲突时身份保留、timer 路径差异、跨树转换不修改参数及客户机已有引用，以及导入失败不改变目标树。CPU 投影使用现有板卡 DTB 检查真实宿主依赖不会进入独立客户机。原实现上的三个确定性失败由 `cargo xtask test --since` 取得；最终使用同一入口、`cargo xtask clippy --package axvm` 和受影响的 AArch64、RISC-V QEMU 启动用例验证。

修改只影响启动期的固件构造，不增加运行期查询或全局状态。导入使用一次目标树副本和节点对应表，空间与树大小成正比；未进行启动性能基准。没有配置格式或持久状态迁移，回滚代码即可恢复旧行为，但旧版会恢复编号冲突风险。实体板卡与独立客户机组合仍需要实际运行证据；QEMU 结果不能替代板卡验证。

2026-10-02 本地验证通过：`cargo xtask test --since 72a6a69f3` 选中 `axvm` 和 `virtualization-tests`，两个 AxVM 功能组合分别通过 453、470 项测试，关联集成测试通过 10 项；`cargo xtask clippy --package axvm` 的 25 个组合全部通过。`cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case smoke` 与对应 `--arch riscv64` 命令均以退出码 0 完成，各通过 1/1 用例，覆盖双 Linux 客户机启动和磁盘隔离。实体板卡未运行。

## 3. 固件声明与资源授权

`GuestConfig.kernel.memory_regions` 和设备选择器是启动配置的资源授权来源，DTB 属性只能描述这些资源。继续由 DTB 补齐映射会让固件输入改变授权；另建一套授权配置会重复现有边界。本轮复用现有内存配置和设备选择器，并收紧解析与输出检查。

### 3.1 保留内存

`/reserved-memory` 的静态 `reg` 若未被客户机内存记录完整覆盖，只记录告警并保留原节点，不阻止启动，也不自动追加 `MapReserved`。配置阶段检查固定 GPA；存在 `MapIdentical` 时覆盖诊断推迟到 `patch_guest_fdt_for_runtime()`，使用实际 `VMMemoryRegion` 地址。动态 `size` 保留区由客户机在自己的 RAM 内分配，不新增宿主映射。配置中的显式 `MapReserved` 继续表达管理员授权的 GPA=HPA 区间，并按配置权限建立映射。

保留区不等于物理授权，依据 [reserved-memory binding](https://raw.githubusercontent.com/devicetree-org/dt-schema/main/dtschema/schemas/reserved-memory/reserved-memory.yaml)。覆盖检查不是完整的地址空间授权检查：`GuestRegionPlanner` 在 passthrough 模式下还建立整体身份映射，并为已占用或排除的区间挖洞，不能把 `memory_regions` 当成全部映射清单。兼容策略保留已有地址空间行为，实际可访问性由映射规划决定；若设备确实需要当前策略未映射的区间，仍需补显式配置。地址溢出、错误 `reg` 布局和不支持的地址转换继续报错。

### 3.2 依赖与禁用

引用布局统一由 `references::reference_offsets()` 解码；导入拒绝未知绑定，发现路径保留未知普通属性，但已知引用必须完整有效。依赖闭包可以补齐描述性节点；具有寄存器、总线映射或中断资源的 provider 必须被设备选择器授权。参数值即使等于其他节点的 phandle，也不会授予那个节点的 MMIO 或 IRQ。

`device::provider_nodes()` 按引用绑定保留 provider 的描述性子节点：`*-supply` 保留 [regulator binding](https://raw.githubusercontent.com/torvalds/linux/v6.12/Documentation/devicetree/bindings/regulator/regulator.yaml) 定义的 `regulator-state-standby`、`regulator-state-mem`、`regulator-state-disk`；`operating-points-v2` 保留 [OPP binding](https://raw.githubusercontent.com/torvalds/linux/v6.12/Documentation/devicetree/bindings/opp/opp-v2-base.yaml) 中的 OPP 条目，并继续解析其 `required-opps` 引用。其他后代不会因为父节点被引用而自动进入客户机树。描述性子节点也经过资源授权检查；显式排除可选状态子节点时，保留仍被引用的无资源父节点身份。

`interrupt::is_machine_interrupt_provider()` 按受支持的完整 `compatible` 识别 GIC、ITS、PLIC 及已实现的 LoongArch 机器控制器。派生筛选、禁用检查、宿主资源保护与直通 IRQ 提取共用这一判断，不根据 `interrupt-controller`、`intc` 等节点名前缀或 compatible 子串推断机器角色。普通外设中断控制器遵循设备选择与显式禁用配置；机器控制器继续由设备模型管理。

显式 `devices.disabled` 在派生与提供 DTB 两条路径都删除整棵子树，剩余启用消费者引用被删除 provider 时启动失败；别名随目标清理。机器控制器、定时器等替换节点不因物理直通排除而消失，但用户显式禁用必需机器节点属于配置冲突。检查与修改在临时树上完成，失败不发布 DTB。最终运行期拒绝设备安装重新引入配置禁用的节点。固件原有 `status = "disabled"` 的图端点可以保留引用身份，但不取得 MMIO/IRQ；这与用户显式禁止 provider 的配置语义不同。

本轮不改变配置格式，不引入长期状态或后台任务。启动扫描和索引只用于当前固件，增加的临时内存随构造结束释放。回滚代码会恢复隐式授权问题，兼容旧板卡采用覆盖不足告警，保留区声明不会扩展授权。进程内真实 FDT 测试覆盖错误授权、参数碰撞和禁用一致性，QEMU 验证启动装配；实体板卡仍需实机验证。


### 3.3 CPU 引用投影与迁移边界

`prune_cpu_references()` 在宿主派生 DTB 中按同一绑定解析器裁剪 PLIC 上下文与 thermal cooling-map 中未分配 CPU 的条目；空 cooling-map 删除，整机 `cpu-map` 不再保留。显式客户机 DTB 在 `cpu::project_cpus()` 完成后也经过该步骤：执行属性投影删除 `#cooling-cells`，因此即使保留 CPU phandle，也移除指向该 CPU 的 cooling-device 条目。混合 cooling-map 中的风扇等非 CPU 条目及其参数保持原样，解码使用替换前客户机树的绑定布局，不从宿主 phandle 推断引用。普通设备 provider 丢失仍然报错。真实 OrangePi DTB 测试会继续经过资源提取使用的依赖解析入口，防止只检查编码成功而漏掉筛选后的悬空引用。

`disabled::apply()` 通过 `timer::is_architectural_timer_node()` 保护 `arm,armv8-timer` 和 `arm,armv7-timer`，不根据 `timer@...` 名称禁止用户排除普通板级定时器。`is_machine_timer_node()` 继续用于原有物理资源保护和机器定时器安装时的固件清理；允许显式禁用普通定时器不会新增物理直通能力。

仓库 OrangePi 宿主 DTB 中的固定 CMA 区间为 `[0x10000000, 0x20000000)`，ramoops 为 `[0x110000, 0x1f0000)`。旧 `linux-smp1.toml`、`starry-smp1.toml` 的宿主整树派生路径保留这些节点时，覆盖不足只告警，不再要求先迁移配置才能启动。节点及引用保持原样，也不增加物理授权；不能据此断言相应驱动一定可用，实际映射及缓存属性仍由原有地址空间策略决定。新的 virtualized virtio-blk 配置不采用宿主整树直通，ROCK 4D 提供的客户机 DTS 没有这些保留区。

### 3.4 验证结果

2026-10-02，`cargo xtask test --since cb984a697` 选中 `axvm` 和 `virtualization-tests`，两个 AxVM 功能组合分别通过 453、470 项测试，关联集成测试通过 10 项。三个授权回归用例先在旧实现上确定失败，真实 OrangePi 二次解析用例也先复现了 CPU cooling-map 悬空引用，再验证修复。既有保留区差集辅助函数及三个对应测试随隐式映射逻辑删除，覆盖由实际授权校验测试承担。

最终 `cargo xtask clippy --package axvm` 的 25 个组合全部通过。`cargo xtask axvisor test qemu --arch aarch64 --test-group normal --test-case smoke` 及对应 `--arch riscv64` 命令均以退出码 0 完成，各通过 1/1，验证双 Linux 客户机启动及磁盘隔离。`cargo fmt --package axvm` 与差异检查通过；实体板卡未运行，旧板卡保留区设备的实际可用性不在 QEMU 证据覆盖范围内。


兼容性调整后，覆盖不足由拒绝启动改为告警，保留原 DTB 且不追加映射；格式错误与地址溢出的拒绝继续保留。更新后的同一回归测试先在严格版本上失败，再在兼容版本上通过。`cargo xtask test --since 96082412b` 的两个 AxVM 功能组合及关联集成测试分别通过 453、470、10 项；`cargo xtask clippy --package axvm` 通过 25 项，AArch64 QEMU smoke 通过 1/1。OrangePi 实机未运行。


2026-10-02 筛选修复验证：先在 `bbb89d8ee` 的旧实现上复现普通中断控制器强制保留、显式禁用被拒绝和描述性子节点丢失，共三个确定性失败。将最终修改提交为临时工作树快照后，`cargo xtask test --since bbb89d8ee` 的两个 AxVM 功能组合分别通过 455、472 项，关联集成测试通过 10 项；当前工作区与已测源码快照一致，主分支未新增提交。`cargo xtask clippy --package axvm` 通过 25 项，`cargo fmt --package axvm` 和差异检查通过。

AArch64 QEMU smoke 通过 1/1。RISC-V smoke 首次在第二个客户机启动阶段超时（300 秒），修复前 `bbb89d8ee` 的同用例对照通过，随后当前修复的同用例复测通过 1/1，完成双客户机启动和磁盘隔离检查。首次超时原因尚未定位，不能据复测通过认定启动稳定性问题已排除；实体板卡未运行。

2026-10-02 CPU cooling 与定时器禁用修复：增强现有 CPU 投影及定时器安装测试，先在未修复实现上分别确认 cooling-map 残留和普通定时器禁用被拒绝，两种 AxVM 功能组合均出现这两个确定性失败。修复后以隔离工作树提交快照运行 `cargo xtask test --since bbb89d8ee`，两个组合分别通过 455、472 项，关联集成测试通过 10 项；主工作区源码与该快照一致。测试同时验证混合 cooling-map 的非 CPU 条目和参数得到保留、最终树可再次解析依赖，以及架构定时器仍禁止显式禁用。

本轮 `cargo fmt --package axvm`、差异检查及 `cargo xtask clippy --package axvm` 的 25 项检查通过。AArch64 和 RISC-V 的 `cargo xtask axvisor test qemu --arch <arch> --test-group normal --test-case smoke` 均通过 1/1，完成双客户机启动与磁盘隔离检查；实体板卡未运行。此前 RISC-V 超时的根因仍未定位，本轮通过不改变该限制。
