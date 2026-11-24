# LoongArch LVZ 世界切换与二阶段缺页

## 1. 问题与依据

3A6000 的可信 `cpu/guest-entry` 和 AxVM Linux 启动分别暴露了世界切换、
目录编码及缺页退出问题。QEMU 中已有通过结果没有覆盖全部实机语义，
因此验证须同时保留真实寄存器边界和 Linux 用户态行为。

### 1.1 启动与运行故障

`Vcpu::run()` 在分页 Rust 路径提前写 `GTLBC.TGID`，会把后续宿主 TLB
填充标成客户机条目，造成宿主无法命中的重复填充。停点随代码布局变化，
测试打印“进入客户机”不代表已经执行 `ERTN`。

AxVM 的 `LoongArchPTE::new_table()` 原来给目录基址附加 `V/P/MAT` 叶子
属性。真实 `LDDIR` 把目录值作为下一级表的物理基址，因此从错误偏移继续
遍历；Linux 在首条已映射指令上反复发生缺目录退出。

缺目录直接返回宿主还会保留旧 `ESTAT/BADI`。Linux 首次 UART 访问因而可能
携带上一条 CPUCFG 指令，无法按真实 load/store 解码 MMIO。单独清除
`IsTLBR` 和恢复 `DA/PG` 不能补足这条指令信息。

进入 shell 后，原完整用例仍停在 `sleep 1`。每次重入只恢复 `TCFG` 会反复
重置硬件倒计时；IDLE 任务休眠还需要宿主注册负责唤醒，不能依赖已经归还的
客户机硬件定时器。CPU 用例须覆盖反复重入的截止时间及确认后不重复触发。

### 1.2 权威参照与取舍

固定对照 Linux 7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：
`arch/loongarch/kvm/switch.S` 在直映代码中切换共享页表与 TGID；
`kvm/mmu.c::kvm_populate_gpa()` 用 `__pa(child)` 发布目录；
`mm/tlbex.S::handle_tlb_refill` 遍历到无效叶子后重试，由普通异常报告访问类型，
`kvm/exit.c::kvm_handle_rdwr_fault()` 再消费该异常的 BADI。
`kvm/timer.c` 保存 `TCFG/TVAL/ESTAT`，恢复剩余计数并处理过期与 IDLE 唤醒，
为本次定时器切换提供依据。

项目选择恢复同一硬件契约，复用已有 CPU 描述符与普通退出路径。不为缺目录
增加客户机指令复制、第二份 RAM 视图或专用 MMIO 后备；不从物理地址零继续
取表，不把旧 BADI 当成本次访问，也不通过改变超时或轮询处理停顿。

## 2. 机器状态与所有权

`ax-cpu` 拥有本核 LVZ 寄存器切换及页表描述符编码；AxVM 拥有 VM 的映射、
设备与故障处理。内存所有者提供入口地址和物理页表根，不能由 CPU 层猜测
宿主布局、板卡地址或设备归属。

### 2.1 入口与翻译根

`EntryAddresses::new()` 接受所有者准备的执行别名及真实物理 refill 地址。
`TLBRENTRY` 必须写物理地址，高于 PALEN 的位不能作为可用 PC 高位；
`run_guest()` 及普通退出向量通过保留的直映别名执行。`Vcpu::run()` 仍在进入前
拒绝不符合当前分页 PLV0、IRQ-off、coherent DATF/DATM 和 Watch 关闭契约的宿主。

`Vcpu::set_root()` 选择四级 4 KiB 根，`SWITCH_TO_GUEST_TRANSLATION` 安装
相同根到 root `PGDL/PGDH`。`CSRWR` 会把旧 CSR 值回填源寄存器，因此必须
先复制输入，再分别写入；第二次写入不能消费第一次交换出的旧根。
宿主两侧根仍分别保存和恢复。

### 2.2 客户机填充目标

`Vcpu::run()` 在分页 Rust 中选择 `GSTAT.GID`、配置拦截并失效旧客户机翻译，
保持 `GTLBC.TGID=0`。`run_guest()` 完成浮点现场切换后跳入直映机器入口，
该入口安装客户机根及 walker，最后从 `GSTAT.GID` 选择 `GTLBC.TGID` 并
启用 `USETGID`。

从设置 TGID 到 `ERTN`，代码、上下文和后续 refill 均不依赖宿主分页填充。
普通退出先由 `ENTER_HOST_MODE` 清零 TGID，再恢复宿主根和分页 Rust 上下文。
guest ID 继续由原有 vCPU 所有者提供，不增加第二份状态或配置。

### 2.3 二阶段目录

`LoongArchPTE::new_table()` 复用 CPU `Pte::new_table()`，只保存对齐物理地址。
最终叶子仍由 AxVM 的 `MappingFlags` 决定权限、内存属性与大页位。
`LoongArchPTE::present()` 同步复用 CPU 判定，接受合法地址目录和有效叶子，
不再要求目录带叶子 `V` 位。

软件查询和硬件 walker 使用同一描述符规则，不在 `LDDIR` 后额外掩码来兼容
错误目录。页表没有持久化格式；更新代码后重建 VM，按既有映射发布及客户机
翻译失效协议启用新表。

### 2.4 缺目录后的访问异常

`__ax_cpu_lvz_guest_tlb_refill_vector` 继续使用 `AX_CPU_WALK_4K`，在每层检查
空目录。缺目录时把两个 `TLBRELO` 清零，保留原 `TLBREHI.VPPN`，只把页大小
设为 4 KiB，随后与正常叶子共用 `TLBFILL -> 恢复 t0 -> ERTN`。

无效条目让同一客户机指令重试，CPU 产生 load/store/fetch page-invalid
普通异常。该异常提供当前 `ESTAT.ECODE`、`BADV`、`BADI` 和 ERA，再经唯一
`VS=0` 普通入口保存现场、归还 TGID 和宿主翻译。AxVM 的既有普通缺页与
MMIO 分派消费这份拥有值现场，只有设备实际接管后才推进 PC。

refill 不再直接跳回宿主 Rust，也不再手工清 `IsTLBR` 或重建 `DA/PG`；硬件
`ERTN` 完成 refill 模式退出。删除专用 refill-to-host trampoline 和
`GuestContext.guest_refill_alias`，机器偏移仍由 Rust 生成。调用方须重新编译，
不能依赖旧 `GuestContext` 二进制布局或缺目录必然返回 `IsTLBR=1` 的旧假设。

### 2.5 客户机定时器

只恢复 `TCFG` 会把硬件剩余计数重新装载为初值；宿主时钟频繁打断时，较长的
客户机一次性定时器因而不能到期。对照 Linux `kvm/timer.c` 的做法，CPU 退出
保存真实 `TVAL` 及同一常量计数器的采样，然后停用被借出的硬件定时器。
下一次进入从剩余值扣除宿主经过的 ticks，再按 `TCFG -> TVAL` 恢复。

`GuestContext.timer_counter` 与 `gcsr_tval` 共同表示这一份定时状态；模拟器
改写倒计时也须更新采样，不另建世界切换定时器或后台任务。过期一次性计数的
全一哨兵不能重新装载；周期定时器跨过多个周期时保留余下相位并设置 pending TI。
进入先停旧定时器、清旧 TI，再恢复属于本客户机的状态，避免其他绑定的残留。
CPU 层不发布 AxVM 中断、分配资源或调用宿主调度器。现有客户机缺页、设备及
软件模拟分派仍由 VM 所有者负责。

AxVM 的 `wait_for_event()` 通过 `arm_idle_timer()`，在任务上下文把保存的
剩余 ticks 转为宿主截止时间，复用 `GuestTimerRegistration` 唤醒同一 run。
等待结束先取消并等待回调完成，覆盖外部中断和停止与截止时间竞争；已发布 TI
继续留在 run 队列中。硬件执行和 IDLE 休眠期间分别由 CPU 与原宿主注册提供
计时，不增加第二个后台所有者，也不在绑定区调用可休眠服务。

## 3. 验证与边界

验证使用项目入口，并区分功能回归、额外机器观察和 Linux 板测。调试插桩只用于
定位与取证，最终运行恢复原超时、成功标记及客户机全部断言。

### 3.1 CPU 与机器边界

增强同一个 `cpu/guest-entry` 用例：缺目录读取必须返回 load page-invalid、
保留原 load PC 与 BADV，并报告实际 `LD.D`，不能报告残留 HVCL。补上映射后
同指令读取新 backing；原有 FP、CPU anchor、TLS、IRQ/LIE、映射替换、宿主
定时器抢占、pending TI、重入与解绑判定继续保留。

同一用例还以短于客户机周期的间隔反复进入 HVCL，要求一次性和周期定时器在
常量计数器越过截止时间后留下 pending TI。宿主等待属于被测墙钟语义，使用
架构计数器作为明确截止条件；不能因客户机每次只执行很少指令就推迟到期。

额外只读 RSP 观察在真实运行的分页 wrapper 与直映入口核对 TGID，以及两侧
根是否等于传入根。停点由当前 ELF 符号与实际指令定位并核对字节；分别恢复
提前切 TGID 和串联 CSRWR 时，同一观察器必须因契约断言失败。修复后撤断点、
detach，并让原 CPU 用例继续完成。协议、构建或身份失败不计为 red。

这些观察器没有自动接入默认 CI，不能把手工机器检查说成默认门禁覆盖。
最终无调试器 QEMU 与 3A6000 CPU 用例仍须独立执行；普通宿主 QEMU 缺少 LVZ
时使用项目固定 LVZ 环境，不能把环境错误算作回归。

### 3.2 Linux 实板验收

Linux 用例按[3A6000 设计](axvisor-loongson-3a6000-linux.md)运行。
目录问题的定位记录须区分首条指令反复缺页、修复后的实际指令前进，以及后续
设备故障；局部进展或诊断用例退出零不能替代 Linux 用户态通过。

最终保留同一版本两次独立板级启动，实际进入 shell，核对 Linux 7.1 与
LoongArch、确定性计算、RAM 文件写读、定时等待、只读 VirtIO 块设备和 ext4
夹具内容。每轮记录源提交、上传 ELF 和宿主归档摘要、完整日志及本次租约释放。
板卡服务在释放租约时断电，在下一租约串口接入后上电，以此形成独立板级启动。
正常测试不叠加额外复位；加载器失联时先核对在线状态和 boot epoch，只在本任务
已取得的租约内恢复板卡，不操作其他会话。

CPU 与页表修复不外推客户机 SMP、多 VM、设备直通、性能或完整发行版兼容。
