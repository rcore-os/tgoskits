# RK3588 CPU 调频设计

Orange Pi 5 Plus 有三个独立 CPU 调频域：A55 cpu0-3、A76 cpu4-5、A76 cpu6-7，
对应 SCMI 时钟号 0、2、3。每个域共用一个时钟和一条 CPU/mem 电源轨。
`rdif-cpufreq` 描述通用调频域、OPP、限制和硬件错误；
`ax-runtime::cpufreq` 为 ArceOS、StarryOS 和 Axvisor 提供唯一的 governor 工作线程。
RK3588 的 OPP 表筛选、温度规则、DSU 约束和转换顺序位于 `rockchip-soc::rk3588`，
`ax-driver::soc::rockchip` 负责 FDT 探测以及 SCMI、PMIC、OTP、TSADC、GRF 的资源适配。
原 `ax-driver::cpufreq` 公共门面和 StarryOS 校准入口已移除；内核调用方通过
`ax-runtime::cpufreq` 发起串行请求。

调频策略属于通用运行时能力，`ax-runtime` 和 `ax-std` 只依赖
`rdif-cpufreq`，不声明 RK3588 驱动 feature。板卡或测试构建在对应 TOML 的
`features` 中直接选择 `ax-driver/rk3588-cpufreq`；未选择该 feature 时运行时查询返回
`NotSupported`。驱动源码按 `drivers/ax-driver/src/soc/rockchip/cpufreq/` 分层：
`rdif.rs` 是接口适配，`selection.rs` 负责硅片与 OPP 筛选，`transition.rs` 负责
电源、GRF 和 SCMI 转换，`sensors.rs`、`pvtm.rs`、`margin.rs` 与 `board.rs` 提供
独立硬件规则，`pmic_i2c.rs` 和 `pmic_spi.rs` 负责电源轨读写。

## 1. 启动与控制

`ax-runtime::bootstrap` 在设备探测建立启动 OPP 后启动次级 CPU，随后创建唯一的
调频工作线程。该顺序保证硅片分档与运行时调档开始时，三个 CPU 域都已有可确认的
初始频率。

### 1.1 启动顺序

启动过程先注册 `rdif-cpufreq`，再确认启动时钟和电源轨，最后由
`ax-runtime::cpufreq::start` 接管策略；以下流程标出硬件与策略的交接点。

```mermaid
flowchart LR
    A[CPU 设备探测] --> B[注册 rdif-cpufreq]
    B --> C[确认 1008/1200 MHz 启动状态]
    C --> D[所有宿主 CPU 上线]
    D --> E[ax-runtime 工作线程]
    E --> F[OTP/PVTM/温度筛选 OPP]
    F --> G[ondemand 或 performance]
```

探测先注册 CPUFreq 设备，再把三个时钟设到已验证的启动 ring，确认 RK8602、RK8603
和 RK806 电压读回，并将轨电压逐级对齐到 750 mV。设备探测未建立安全状态时查询
返回 `NotReady`；没有设备才返回 `NotSupported`。完成所有宿主 CPU 上线后，运行时从宿主 bootargs
读取 `cpufreq.default_governor=ondemand|performance`。缺省或无效值为 `ondemand`，
无效值另记告警。Axvisor 客户机命令行不控制宿主调频。固定频率调整只供内核调用，
本次没有 StarryOS 用户态 ABI。

### 1.2 调频接口

`rdif-cpufreq::Interface` 是驱动与三套系统共用运行时之间的契约。调用方按调频域
读取 OPP 和限制，由运行时将策略切换、固定频率请求和温度刷新送入同一工作线程。

`rdif-cpufreq::Interface` 按域提供 `domains`、`available_opps`、`current_opp`、
`limits`、`set_frequency` 和 `refresh_limits`。`DomainInfo.cpu_ids` 是调度器逻辑 CPU
编号；适配器通过 `axklib::cpu::resolve_logical_index` 转换设备树中的硬件 ID，
不得假定设备树文档顺序等于逻辑顺序。运行时把策略、固定频率请求和温度刷新串行
送入同一个任务上下文。`current_opp` 是完整读回后的策略状态，实际送达频率须以
绑核 PMU 周期计数和系统计时器测量。

`ondemand` 每 100 ms 采样各域最忙核心的非 idle 比例：达到 80% 升至当前上限，
全部低于 30% 降一档。`performance` 持续请求限制内最高 OPP。温度或 DSU 约束
改变后，驱动先完成必要降档，才公布新限制；固定请求若当前不可用返回
`OppUnavailable`。固定请求被接受后若温度上限降低，工作线程会选择不高于请求的
最高可用 OPP；若大核提高了 A55/DSU 下限，硬件安全下限优先，可能选择高于请求的
最低可用 OPP。首次发生约束偏离时记告警；内核调用方通过 `snapshot` 或
`current_opp` 查询实际 OPP。硬件读回失败返回 `HardwareFailure`，受影响域停止后续写入，
并以 `NotReady` 表示软件已不能证明当前 OPP。任一域调档失败都会关闭三个调频域：
SCMI 写入可能已生效而读回失败，大核实际频率未知时不能再按旧索引降低 A55/DSU。

## 2. 芯片筛选与转换

RK3588 驱动以板卡 DTB、OTP 和 PVTM 决定可用 OPP，再让 SoC 转换状态机依次确认
供电、GRF read margin 与 SCMI 时钟。筛选失败只公布可确认的保守档位，
硬件转换失败则关闭后续写入。

### 2.1 OPP 筛选

`rockchip-soc::rk3588::cpufreq_opp` 负责解析 DT OPP 电压和硬件掩码，
`ax-driver::soc::rockchip::cpufreq::select_domain_opps` 提供 OTP、PVTM 与温度实测值。
只有这两层都确认的档位才进入 `available_opps`。

CPU OPP 来自板卡 DTB 的三个 `operating-points-v2` 表。OTP byte 6 的低五位
`0x0d`、`0x0a` 分别是 M/bin1、J/bin2，其余是标准/bin0。探测双读 OTP 缓存；
bin 非零的 J/M SKU 当前在板级适配层直接返回 `NotReady`，不会进入 PVTM 或 OPP
筛选；随后初始化会关闭 CPUFreq 三个域，保留固件已经建立的时钟/电压状态，接口
对外返回 `NotReady`，而不是宣称这些域已经有可调的低档。DT 中的
`rockchip,pvtm-voltage-sel-hw` 和 `rockchip,pvtm-hw` 仅保留为后续完成 J/M
实体板验证时的输入，不会单凭存在该表就开放高档。标准/bin0 SKU 在 PVTM
表指定的 750 mV 和 1.416/1.608 GHz 测量点取 GRF 样本，按 TSADC 温度
修正后，从 `rockchip,pvtm-voltage-sel` 选择电压档。温度使用三个 CPU OPP 表共同
指定的 `soc-thermal`，即 TSADC 通道 0；它也是 BSP system monitor 的输入。
测量频率必须与这两个
已确认的板级值严格相等，异常 DT 值在 SCMI 升频前拒绝。随后同时匹配
`opp-supported-hw` 型号与电压档掩码，优先读取 `opp-microvolt-L<档>`，
没有该属性才读取普通 `opp-microvolt`。`opp-info` OTP 修正电压仍受 OPP
允许的最大值限制。任一必要输入缺失、格式无效或测量无法恢复启动时钟时，
不发布依赖该输入的高档。

GRF 初始 read margin 写入前，时钟先降至 DT `intermediate-threshold-freq`
规定的 1.008 GHz 并读回，写入后恢复启动频率并读回。A76 PVTM 分档不高于
`rockchip,pvtm-low-len-sel` 时，再按 BSP 发送一次带 `OPP_LENGTH_LOW` 位的
SCMI rate 请求，随后恢复普通 rate；除下述旧固件兼容路径外，任一步失败都不开放
该域高档。SCMI 只返回
请求状态与普通 rate，不能直接读回 PVTPLL length 模式，因此该步骤还必须结合
实体板实际送达频率验证。板卡固件若通过 `PVTPLL_GET_INFO` 声明支持低温配置，
驱动在低温状态切换时调用 `PVTPLL_LOW_TEMP`；未声明支持时不发送该 SMC。

当前 Orange Pi 5 Plus 板上的 BL31 对 `PVTPLL_GET_INFO` 返回 `SMC_UNKNOWN`
(`0xffffffff`)，对带低长度标志的 SCMI rate 返回 `InvalidParameters`。
`configure_pvtpll_length` 仅在 SIP 返回 `SMC_UNKNOWN`、SCMI 对该请求明确返回
`InvalidParameters`，且普通 rate 恢复并读回成功时
使用 BSP 同样的旧固件路径；其他拒绝或读回失败仍关闭该域高档。旧固件不会报告
length 是否生效，因而该例外必须由对应板卡的绑核 PMU 频率测量和领域审查共同约束。

高档当前只对实体板验证过的标准 SKU 分档开放：A55 PVTM 档 0、1，
两组大核 PVTM 档 0、3。J/M SKU 的 CPUFreq 选择当前关闭；其他未认证分档
也不会进入可用列表；
大核未完成 PVTM/GRF 确认时回到 816 MHz。即使 DT
列出更高频率，也不因静态表存在就开放。
板卡适配层的 `board::verified_maximum_hz` 还限制已验证分档的频率：A55 档 0/1
不超过 1.8 GHz，大核档 0 不超过 2.256 GHz，档 3 不超过 2.352 GHz。
`select_domain_opps` 在公布 OPP 前过滤超出本分档实测上限的 DT 行，
使 `performance` 和固定请求不能选择尚未测量的高档。新增分档或提高上限
需补齐对应实体板的轨电压读回和绑核频率测量，再更新这道门控。

### 2.2 OPP 转换

`rockchip-soc::rk3588::cpufreq::transition` 决定调档顺序，RK3588 适配层分别实现
轨电压、GRF read margin 和 SCMI 时钟写入与读回。软件 OPP 索引只在整个转换完成
后更新，因此中途失败不会被报告为已达到目标档。

三域使用各自的轨：RK806 DCDC2 经 SPI2 驱动 A55，RK8602/8603 经 I2C0 驱动
两个大核域。调压每步至多 25 mV，读回选择码并等待稳定。CPU 与 mem supply
在该板卡指向同一物理轨，只写一次。GRF read margin 按电压表设置并读回；
SCMI 设置也必须读回目标 ring。SoC 层转换状态机规定：

RK8602、RK8603 在轨电压读写开放前读取 ID1 寄存器，只有 die ID 低四位为
`0xa` 才按板卡电压编码访问。PMIC 选择码读回确认了寄存器状态，实际轨电压仍
依赖器件正常工作。

| 转换 | 第一步 | 第二步 | 部分失败后的处理 |
| --- | --- | --- | --- |
| 升档 | 确认供电和 read margin | 确认 SCMI 时钟 | 停止；可能过压，不提交 OPP |
| 降档 | 确认 SCMI 时钟 | 确认 read margin 和供电 | 停止；可能过压，不提交 OPP |

RK3588 大核频率对 A55/DSU 有最低频率要求。按 BSP 规则，大核目标频率的
80% 向下取整到 100 MHz；升大核前先满足小核下限，降小核前先检查两个大核
当前频率。不能满足该要求的大核档从当前限制中排除。

## 3. 温度与失效

`refresh_limits` 在每轮策略计算前读取真实 TSADC，并由 `ThermalState` 应用温度迟滞。
必要的降档或升压完成后才允许后续策略请求观察新状态；读回错误会关闭受影响的
调频能力。

### 3.1 温度限制

`ThermalState::update` 维护高低温两个独立迟滞条件，`refresh_limits` 将它们映射到
每域的频率上限和有效电压，并在超限时先执行降档。

TSADC 初始化七个通道的 120 °C 硬件关机比较器及 CRU 路由；调频读取
`soc-thermal` 通道 0，并在每次读取时确认该通道及关机路由仍有效。低于
10 °C 启用 750 mV 电压下限，超过
15 °C 解除；高于 85 °C 将 A55 限到 1.608 GHz、大核限到 2.208 GHz，
低于 80 °C 恢复。温度丢失时，电压使用 750 mV 下限，频率不超过已确认的
A55 1008 MHz、大核 1200 MHz；未完成大核分档确认时仅开放 816 MHz。
每次工作线程轮询先更新这些限制再处理请求。

对于固件明确声明支持的 PVTPLL 低温模式，进入低温时先确认电压下限，再发送
`PVTPLL_LOW_TEMP(1)`；退出时先发送 `PVTPLL_LOW_TEMP(0)`，再允许降低轨电压。
SMC 只有成功状态，没有独立的应用状态读回；返回失败则关闭该域后续调频。

### 3.2 失效边界

`check_ready` 只开放读回已确认的调频域；`mark_domain_failed` 在转换状态不再可信时
关闭三个域，避免大核频率未知后继续按旧 DSU 约束调小核。
若 A55 的 PVTM/OPP 筛选或域确认失败，`initialize_post_boot` 在 SCMI 读回仍可用时
先将未筛选的两组大核降至 816 MHz 并读回，再恢复 A55 启动时钟、检查域状态；
任一步失败都关闭全部调频域：
750 mV 下的 A55 1.008 GHz ring 在实体板上可能实际送达约 1.15 GHz，不能作为
已校准的固定 OPP 对外公布。

OTP/PVTM/传感器、PMIC 读回、SCMI 时钟或 GRF 确认失败均不得通过猜测软件
索引开放档位。RK806 已在实体板上读回 buck2 选择码；任何无法再次读回的
A55 实例仍不能进入需升压的 OPP。板卡 DTB 静态最高 A55 1.8 GHz、
大核 2.4 GHz，并不意味着每颗芯片都能使用。标准 SKU 的某一 PVTM 档
最高为 2.256 GHz，较高档可选择 2.352 GHz；标准 SKU 实际以 OTP、PVTM 和
普通电压档表为准；J/M SKU 走前述 fail-closed 路径，CPUFreq 接口保持
`NotReady`，不公布临时的低档 fallback。

`initialize_post_boot` 对未通过 PVTM 筛选的大核不继续使用 1.2 GHz
启动环频率：此时尚未确认该芯片的 GRF read margin 和实际送达频率。
驱动将其降到固件原有的 816 MHz 环频并读回，`LIMITED_FALLBACK` 仅公布
这一个 750 mV 档位，不再尝试改变未经确认的供电或 GRF。Orange Pi 机器人板的
A55 PVTM 档 3 没有高档测量；原先 750 mV 大核轨配 1.2 GHz 环频时，
机器人测试在第一个 NPU 性能窗口后卡住。将大核轨调整到标准 SKU 的
675 mV 可恢复流程，但缺少该分档的 GRF read margin 确认，因此不作为生产
回退方案，也不构成开放 PVTM 档 3 高档的证据。

## 4. 验证与开放

主机测试验证纯规则与参数解析；板卡测试验证电源、时钟和调度器共同工作时的
实际结果。测试只证明覆盖到的芯片分档与温度场景，新增分档仍需另行测量。

### 4.1 自动化验证

`cargo xtask test` 运行 `rockchip-soc` 的 OPP 与转换规则测试及 `ax-runtime` 的
CPUFreq 策略测试。ArceOS 的 `cpufreq` 板卡用例补充三域并发调档和 PMU 实测。

主机测试覆盖 OPP 双掩码与电压选择、PVTM 分档、10/15 °C 和 85/80 °C
迟滞、DSU 下限、转换每一步失败时停止，以及启动参数解析。板卡路径是
`cargo xtask arceos test board --board orangepi-5-plus --test-case cpufreq`：
用例读取三个 RDIF 域，设置最高/最低档，跨核同时请求两个档位，再在八个
逻辑 CPU 上用 PMU 周期计数与 CNTVCT 测量实际送达频率。板卡测试专用的
`rk3588-cpufreq-thermal-test` 特性可叠加温度限制：真实 TSADC 每次仍须成功
读取，真实高温和传感器故障不能被模拟温度覆盖。用例按 86、80、79 °C 与
9、15、16 °C 顺序验证限频降档、迟滞和恢复，以及低温电压下限与实际频率。
芯片分档未开放全部三个高档域时会记录 `THERMAL_SKIP_UNREADY`，不把该板
当作高档温度测试通过。QEMU 结果仅验证接口与启动，不作为实体板频率证据。

### 4.2 板卡结果

以下数据来自 Orange Pi 5 Plus 实体板运行，包含两种 PVTM 分档。
频率以绑核 PMU 周期计数与系统计时器换算，轨电压由 PMIC 选择码读回确认。

2026-09-28 的电源时钟修正后，`OrangePi-5-Plus-2` 和最终差异上的
`OrangePi-5-Plus-1` 均通过 ArceOS `cpufreq` 用例：两板都是 OTP 标准 SKU、
A55 PVTM 档 1、两个大核档 3，筛选上限为 1.8/2.352/2.352 GHz。
最终运行在 `OrangePi-5-Plus-1` 的最高档绑核实测约
1.828/2.241/2.252 GHz，三域 408 MHz 档约 396 MHz；三域并发请求和
合成高低温限制得到 `CPU_CPUFREQ_THERMAL_OK`、`CPU_CPUFREQ_OK`。
两板的 `PVTPLL_GET_INFO`
返回 `SMC_UNKNOWN`，低长度 SCMI 请求返回 `InvalidParameters`，因此本次测量
覆盖的是普通 rate 的旧固件兼容路径；不构成新固件接受低长度命令的证据。

2026-09-24 的 OrangePi-5-Plus-3 运行通过：OTP 标准 SKU，PVTM 分档
A55 0、大核 0；OPP 上限 A55 1.8 GHz、大核 2.256 GHz。板上绑核测量
A55 最高档约 1.783 GHz，大核两域约 2.163-2.171 GHz；三域最低
408 MHz 档测得约 396 MHz。RK806 在该板确认 750、675 和 950 mV
写入读回；两个大核轨在 675 mV 与 1.0 V 间完成升降。精确原始日志保留
在本次本地验证记录中。另一次 `OrangePi-5-Plus-1` 运行通过：标准 SKU，
PVTM A55 档 1、大核档 3，选择上限为 1.8/2.352/2.352 GHz；绑核测得
A55 约 1.827 GHz，大核约 2.241-2.252 GHz，三域最低档约 396 MHz。
两块板均确认升降档后的轨电压读回。最终代码再次在
`OrangePi-5-Plus-1` 通过 ArceOS 调频板卡用例：三域最高、最低及并发请求后，
八个核心实测最高约 1.828/2.243/2.254 GHz，最低约 396 MHz。
同一型号的板卡在受控温度用例中完成三域热限制降档与恢复，并确认低温下
750 mV 轨电压读回和约 396 MHz 的实际频率。此项是合成温度的电源、时钟和
软件策略闭环测试，不是实体芯片在 9 °C 或 86 °C 环境下的热稳定性证明。
StarryOS 的 `native-hardware-smoke` 在 `OrangePi-5-Plus-3` 通过，宿主启动
`Ondemand`，筛得 1.8/2.256/2.256 GHz；Axvisor 的
`orangepi-5-plus-linux` 冒烟用例在 `OrangePi-5-Plus-2` 通过，宿主启动
`Ondemand`，筛得 1.8/2.352/2.352 GHz，Linux 客户机进入 shell。
这两项启动用例没有逐簇切档与绑核频率测量；该行为由 ArceOS 板卡用例验证。
实际环境中的高低温切换尚未完成，常温及合成温度结果不能替代该项证据。

### 4.3 合入门禁

实体板测量证明当前两组标准 SKU 分档的常温行为，合入前仍需独立审查高档的
电源、时钟与失效边界；真实环境的高低温稳定性也尚无证据。

高于先前保守档位的 OPP 在合入前须由合格 RK3588 电源/时钟领域审查人确认
供电范围、PVTPLL/固件要求、read margin、DSU 时序、失败状态及板卡测量。
该审查是独立门禁，测试通过不代表审查完成。
