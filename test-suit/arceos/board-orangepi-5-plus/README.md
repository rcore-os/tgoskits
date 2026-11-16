# OrangePi 5 Plus板测

本目录集中登记普通`OrangePi-5-Plus`板测，采用normal入口。默认执行两批、共两次启动，CI与本地使用同一清单。

```text
board-orangepi-5-plus/
├── suite.toml                    唯一启动顺序和成员登记
├── kernel/                       第1次启动：标准库环境
│   ├── build-*.toml              目标、八核上限、日志
│   └── board-*.toml              板型、超时、最终结果
├── pmu-user/                     第2次启动：freestanding、无TLS
│   ├── build-*.toml
│   └── board-*.toml
└── README.md

kernel                            3项、1次启动
├── pmu                           计数器生命周期、逐CPU溢出IRQ
├── ipi                           逐核任务、投递应答、idle、自IPI、并发及截止期
└── cpufreq                       三时钟域、调频、测频、并发请求及温控
pmu-user                          1项、1次启动
└── user-entry                    EL0进入、PMU权限、现场与地址空间恢复
```

## 登记与实现

`suite.toml`顶部`run_order`决定启动顺序，`[batches.NAME]`下嵌套`[[batches.NAME.tests]]`决定成员顺序。每项登记ID、名称、唯一实现入口、unit/result返回契约及所需features；批次关联构建/运行配置，路径相对本目录。Cargo只负责依赖，不复制成员列表。

| 实现 | 位置 | 复用方式 |
| --- | --- | --- |
| PMU | [共享CPU库](../cpu/src/pmu/mod.rs) | ARM QEMU和板端共用 |
| IPI | [Rust套件](../rust/src/task/ipi.rs) | QEMU和板端共用 |
| cpufreq | [RK3588专项](../cpu/src/board/orangepi-5-plus/cpufreq.rs) | 仅对应板端 |
| 用户态 | [共享CPU库](../cpu/src/user_entry/mod.rs) | ARM QEMU和板端共用 |

kernel复用`arceos-test-suit`标准库入口及原runner。PMU/cpufreq正常返回后转Ok，IPI保留原Result；每项保留线程身份、亲和性和调度策略恢复。pmu-user复用`arceos-cpu-normal-freestanding`和原共用main，无新增包或main。

host的`axbuild/src/test/normal/arceos/board.rs`按清单发现、选择、构建、装载及汇总；`board/catalog.rs`由宿主和两个绑定生成器共用，`board/bindings.rs`生成实际调用。清单存在时不再扫描本板目录形成另一份测试身份。

## 功能判定

PMU保留原计数器范围、启停保持、溢出清除及每核两次真实IRQ检查。IPI保留原普通任务逐核执行、逐远端hard call、自claim、本地IRQ状态、idle唤醒、三路并发及等待者截止期检查。它们不能由启动成功或CPU数量字段替代。

cpufreq断言8核和三个RK3588时钟域，切换OPP并以PMU周期数/架构计时器40ms采样测量实际频率，误差门限12%；保留共享域并发和温控检查。温控通过受控温度输入验证真实调频/电压执行，不表示真实加热或传感器校准。

- `CPU_CPUFREQ_THERMAL_OK`：完整温控路径通过。
- `CPU_CPUFREQ_THERMAL_SKIP_UNREADY domains=... fallback=...`：原允许的未就绪/安全OPP路径，不报为完整温控。

两条路径最终均须正常返回；报告分开记录。pmu-user保留逐核真实EL0、PMU权限、寄存器/IRQ现场、地址空间与ASID生命周期检查。

整批成员顺序执行，失败立即中止。kernel最终标记为`BOARD_BATCH_OK batch=kernel selection=all`，单项的selection为实际成员；pmu-user为`BOARD_BATCH_OK batch=pmu-user selection=all`。中间阶段成功不能提前结束外层板测，panic或单项失败向外层返回非零。声明式批次只接受一个被动完成步骤；交互命令或多个宿主步骤会显式报错，不能被完成标记替换丢弃。

boot独立测试和原三个普通单项配置目录已删除；Hello应用保留。kernel实际成功已包含基本启动及应用输出证明，默认不再重复启动Hello。

## 使用

```bash
# 展示两批、成员及启动次数
cargo xtask arceos test board --board orangepi-5-plus --list

# 默认两次启动；与CI相同
cargo xtask arceos test board --board orangepi-5-plus

# 普通三项一次启动
cargo xtask arceos test board --board orangepi-5-plus --test-case kernel

# 单项诊断，只运行IPI
cargo xtask arceos test board --board orangepi-5-plus --test-case ipi

# 同批成员合并为一次启动，仍按清单顺序执行
cargo xtask arceos test board --board orangepi-5-plus --test-case cpufreq,pmu,ipi

# 独立用户态批次
cargo xtask arceos test board --board orangepi-5-plus --test-case pmu-user
```

同批重复名称去重；同时选择批次与其成员时按整批执行。单项选择仍开启该兼容批次所需features并集，但只生成选中函数的调用。build TOML中的`features=[]`不表示未开启测试feature。清单、批次、成员选择和真实features共同进入构建身份，防止整批/单项混用产物。boot已不再可选。

人工运行显式增加`--server <host> --port <port>`，不改CI服务器或共享默认配置。完成后核对自己的会话已释放，不用其他任务并发变化的资源池总数代替租约证据。

## 新增、修改和删除

先遵守[启动复用约束](../../../.agents/skills/test-quality/SKILL.md#36-优先复用启动)。兼容功能加入现有批次，所需features取并集，不能只因feature不同就独立启动。新增独立批次须说明真实不兼容条件或判定差异；不能盲目开启互斥feature、改变关闭状态验证，或把预期终止混入普通批次。

1. 在功能原所属库添加或修改唯一实现，不在板目录复制函数。
2. 在本目录清单登记成员/依赖；需要新批次时同步run_order、build/board配置。
3. 检查单项与整批选择，确认依赖feature联动不会额外执行函数，记录功能数/批次数/启动数。
4. 同步CI的成员源码路由；IPI变化选kernel及原Rust QEMU，PMU变化选相关CPU QEMU和kernel，板端cpufreq变化不触发无关QEMU。
5. 实板验证全部成员、失败中止及恢复；完整温控与安全回退分开记录。删成员时检查专属依赖/配置/路由，删空批次时同步删除顺序和CI身份。

CPU/Rust原QEMU清单仍由各自目录维护，不将板端成员重复加入QEMU默认启动。详见[CPU规范](../cpu/README.md)与[Rust规范](../rust/README.md)。
