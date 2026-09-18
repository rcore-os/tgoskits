# PWM 通用驱动与 BSP 接入

## 1. 能力边界

Starry 固定启用 `ax-driver/pwm`，包含 Rockchip 和 SG200x 两种硬件后端。没有 PWM 控制器的平台仍提供空的 `/sys/class/pwm`。其他 Starry 功能开关保持原有职责。

### 1.1 所有权与分层

`rdif-pwm::Interface` 定义完整状态提交、硬件状态读取和禁用。`RockchipPwm`、`Sg200xPwm` 独占各自寄存器窗口，使用 `tock-registers` 表达布局与位域。构造接口要求映射持续有效且没有并行别名访问；核心不包含设备树、内核锁、物理地址或任务接口。Rockchip 所需的有界延时通过函数能力注入。

`ax-driver` 探测设备树并经 `axklib::mmio::ioremap_raw` 取得内核生命周期映射，完成时钟准备后才发布 `rdif_pwm::Pwm`。`rdrive` 是唯一设备登记来源，按固件枚举顺序生成设备 ID；Starry 按累计通道数分配 sysfs 编号，因此 SG2002 的四组控制器为 `pwmchip0/4/8/12`。无资源、短映射或无效时钟导致探测失败并产生诊断，不猜测固定地址或频率。

### 1.2 请求与实际状态

`PwmState` 的 `period_ns`、`duty_ns` 使用纳秒，`polarity` 定义有效电平，`enabled` 控制输出。`get_state()` 读取硬件量化结果，不返回请求缓存。禁用状态的波形参数没有有效输出含义；SG2002 的 BSP 停止哨兵 `PERIOD=1/HLPERIOD=2` 回读为禁用且占空时间为零。

Starry 的 `PwmChannel::requested` 保存成功请求，初始值来自硬件。sysfs 的四个属性返回请求值，与硬件量化结果分开。禁用请求忽略周期与占空比，允许逐字段暂存；启用时由控制器校验有效周期和占空比。驱动只在校验全部成功后写寄存器，失败不会发布候选缓存。

## 2. BSP 硬件语义

硬件依据固定为两套本地 BSP，不能用主线驱动替换其舍入、时钟编号或寄存器顺序。通用 sysfs 语义单独参照本地 Linux v7.1 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。

### 2.1 Rockchip

依据 Orange Pi `orange-pi-6.1-rk35xx` 提交 `fb528a6014381c12a129e4f5e33c8034d46ad25e` 的 `drivers/pwm/pwm-rockchip.c`，支持当前设备树使用的 RK3328 兼容 v3 控制器。`rockchip_pwm_config_v1()` 对周期和占空比采用 `DIV_ROUND_CLOSEST_ULL`；更新期间设置 LOCK，等待十个输入时钟周期后提交极性并解锁。适配层用 `axklib::time::busy_wait` 满足等待契约。

normal 对应 `DUTY_POSITIVE | INACTIVE_NEGATIVE`，inversed 对应相反电平。旧 Rust 实现将 normal 编成 inversed，本次修正这一映射；依赖旧错误极性的外部消费者应明确请求 `inversed`。驱动设置连续、左对齐、非缩放时钟输出，不支持捕获或单次脉冲；初次探测保留已有波形，不重置计数器。时钟读取复用 RK3588 CRU。`pwm_get_rate()` 按 BSP 读取 GPLL/CPLL、源选择和实际分频，不把 100/50 MHz 源名称当作固定频率，也不强制切换到 24 MHz。`pwm_enable_parent()` 启用选中的源门控，并修正既有 PWM1、PWM3、PMU PWM 门控表与 BSP 不符的问题。

### 2.2 SG2002

依据 LicheeRV Nano BSP 提交 `d4003f15b35d43ad4842f427050ab2bba0114fa5` 的 `osdrv/interdrv/v2/pwm/pwm.c`。先将周期换算成整数 tick，再用已舍入周期计算有效时间；限制有效时间为 `1..period_ticks-1`，因此不承诺精确的 0%/100%。驱动将支持范围保守限制在 `2..=(1<<30)-1` tick，避免 BSP 原始整数运算的溢出和下溢。BSP 未显式说明计数器位宽，此上界是当前支持范围，不作为硬件最大位宽的结论。极性寄存器只修改 BSP 已确认的通道位，保留其他位。

`Sg200xPwm::apply()` 按 BSP 的极性、周期、HLPERIOD、停止、输出使能、重新启动顺序配置；不承诺无毛刺更新。`disable()` 按 BSP 写入停止哨兵。共享寄存器采用类型化读改写，保留其他通道的 OE、启动和极性；不会照搬 BSP 覆盖整个寄存器、可能影响其他通道的写法。

### 2.3 CV181x 时钟

`Cv181xClock` 依据同 BSP 的 `linux_5.10/drivers/clk/cvitek/clk-cv181x.c`，接受厂商时钟编号 51 和 143。通过设备树取得固定晶振，读取 PWM 旁路、源选择和分频；父源覆盖 FPLL 与 MIPIMPLL/DISPPLL 合成路径，保留 BSP 的复位分频值 10 和零分频直通规则。无效 PLL 分母、源选择或合成器分母返回错误。

该提供者只启用 PWM 源与外设门控。PLL、源选择和分频保持固件配置，`set_rate()` 只接受与当前频率相同的请求；不会改变其他设备使用的共享 PLL。门控和永久 MMIO 映射随内核存活，无热拔插和运行期调频。其他 CV181x 时钟编号明确拒绝，本次不移植完整时钟树。

## 3. sysfs 状态与迁移

PWM 控制由每控制器互斥锁串行化，锁顺序为 sysfs 控制器状态、rdrive 设备、时钟提供者。硬件回调不反向进入 sysfs，也不从中断调用这些控制接口。

### 3.1 失败与导出

`write_attribute()` 先构造候选 `PwmState`，由 `PwmChannel::apply()` 调用设备 `apply()`，成功后才覆盖 `requested`。重复 export 返回 `EBUSY`；未导出或越界通道的 unexport 返回 `ENODEV`；取消导出先禁用硬件，失败则保留导出状态。属性操作重新核实通道仍已导出，并校验 `generation`。取消导出后，旧属性描述符返回 `ENODEV`，重新导出不会使旧描述符恢复访问新通道实例。

`CHIPS` 只在首次使用时取得启动后设备快照，不另建厂商发现表。旧的 Starry SG2002 直接寄存器路径及 Rockchip 设备树匹配路径已删除，SG2002 的 `phys_to_virt` 映射缺陷也随之消除。

### 3.2 构建与回滚

`starry-kernel` 和 `starryos` 不再接受 `rk3588-pwm`，板卡构建配置同步删除该 feature；`rdif-pwm` 和 `ax-driver/pwm` 是内核必选依赖。回滚须同时回滚源码、Cargo 配置和板卡配置，不保留旧 feature 别名。新增软件包加入工作区和宿主测试清单；不改变持久磁盘格式。

## 4. 验证

宿主测试证明算法、状态及寄存器访问契约；硬件时序与引脚波形仍需板卡和测量设备。修改涉及 MMIO 安全边界，合入前需领域审查，测试不替代安全证明。

### 4.1 回归与入口

Rockchip 极性回归在旧实现上失败（实际 `0x10`、预期 `0x08`），同一用例在修复后通过。RK3588 时钟回归先在错误 PWM1 门控写入上失败，修复后验证实际源分频、晶振选择和无效源拒绝。SG200x 用例验证两阶段量化、端点限制、失败无写入及通道隔离；CV181x 用例验证父源、分频、门控和无效配置拒绝。

缓存事务测试在 `PwmChannel::apply()` 的硬件接口边界注入确定性的时钟资源失败，验证请求状态保持及后续成功提交；不替换操作系统锁或任务运行时。

`test-pwm-sysfs` 在 QEMU 要求空 PWM 类目录；以 `--hardware` 运行时要求真实控制器，验证导出、失败写入回读、极性、更新和取消导出。Orange Pi 与 LicheeRV Nano 通过同一 C 程序执行，临时文件由板卡会话传送，不修改 Linux 根文件系统。构建、静态检查、宿主测试和系统测试都使用项目 `cargo xtask` 入口，实际运行结果另行记录。

### 4.2 硬件验收边界

sysfs 请求回读只能证明状态一致性，不能证明真实输出频率、占空比或极性。实际波形应在正确引脚复用下与同板 BSP Linux 对照，并覆盖连续输出、运行中更新及禁用。本次用户确认没有可用的示波器或逻辑分析仪，实际波形验证未完成。

### 4.3 本次验证记录

2026-09-17 的验证区分组件规则、内核装配与实体输出。板卡未执行到用户态用例时，不把构建或启动结果记为 PWM 功能通过。

| 验证 | 结果与证据 |
| --- | --- |
| `cargo fmt`、`git diff --check` | 通过 |
| `cargo xtask test` | 67 个软件包全部通过，更新到 `5381bea70a` 基线后再次通过；包含新增驱动、时钟及 Starry 缓存事务测试 |
| Rockchip 极性红绿 | 旧实现 normal 为 `0x10`，预期 `0x08`；修正后通过 |
| RK3588 时钟红绿 | 旧 PWM1 门控写入 `0x80000`，BSP 预期 `0x100000`；修正后通过 |
| 缓存事务红绿 | 临时恢复提交前写缓存，测试确定性失败：缓存被改为 `300000/true/Inversed`；恢复提交成功后发布，原测试通过 |
| 定向 clippy | 首轮 7 包、155 项通过；补充时钟和 SG200x 驱动 2 项通过；最终内核四架构 88 项复查全部通过 |
| QEMU 空类目录 | AArch64、RISC-V、LoongArch、x86_64 全部构建并运行通过，各 1/1，用例输出 `PWM_SYSFS_PASSED`；更新基线后 x86_64 再次通过 |
| Orange Pi 5 Plus | OrangePi-5-Plus-3 实板通过；直接 openat/read/write 验证发现、导出、配置、启停、极性、更新、错误后缓存与旧描述符失效，最外层任务成功 |
| LicheeRV Nano SG2002 | 已构建；U-Boot 报 `Could not initialize PHY ethernet@4070000`，网络加载超时，未进入 Starry 用例 |
| 波形测量 | 用户确认没有测量设备，未完成 |

日志保存在本次工作环境的 `/tmp/pwm-std-final.log`、`/tmp/pwm-std-rebased.log`、`/tmp/pwm-cache-host-red.log`、`/tmp/pwm-clippy-final.log`、`/tmp/pwm-clippy-clock-final.log`、`/tmp/pwm-clippy-kernel-final.log`、`/tmp/pwm-qemu-final-<arch>.log`、`/tmp/pwm-qemu-rebased-x86_64.log`、`/tmp/pwm-board-rk-verified.log` 和 `/tmp/pwm-board-sg.log`。宿主寄存器存储测试不证明 MMIO 总线时序或引脚波形。

### 4.4 Linux 兼容性范围

通用状态有效性、候选状态提交与请求回读依据上述 Linux 提交。项目明确保留累计通道编号；`PwmChannel::unexport()` 延续 Starry 的先禁用再取消导出行为，而 Linux `pwm_unexport_child()` 调用 `pwm_put()` 本身不保证禁用输出。数字输入当前支持十进制，未宣称覆盖 Linux `kstrtou64(..., 0, ...)` 的全部进制及溢出错误语义。

SG2002 补验及后续复验使用 `cargo xtask starry test board --board orangepi-5-plus -c pwm-sysfs` 和 `cargo xtask starry test board --board licheerv-nano-sg2002 -c pwm-sysfs`，再按 4.2 节测量波形。旧属性描述符在取消导出及重新导出后都应返回 `ENODEV`，硬件错误不得污染请求缓存；这些软件不变量已有组件测试和 Orange Pi 实板系统调用验证，SG2002 的真实系统调用路径仍待验证。
