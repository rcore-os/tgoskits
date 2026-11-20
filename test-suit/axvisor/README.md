# AxVisor 测试

`qemu/`与`board/`直接承接自动化功能测试；交互演示与手动启动配置位于 `apps/suite/axvisor`。目录整理和共享启动不改变原测试步骤、断言、压力参数及未登记项目的状态。

## 目录与启动边界

```text
axvisor/
├── qemu/
│   ├── aarch64/<启动组>/
│   ├── riscv64/<启动组>/
│   ├── loongarch64/<启动组>/
│   ├── common/web-ui/                  仅QEMU跨架构共享的原Web实现
│   └── x86_64/
│       ├── vmx/<启动组>/
│       ├── svm/<启动组>/
│       └── common/guests/              两个后端实际共用的客机配置
└── board/<板型>/<启动组>/
```

一个运行配置对应一次宿主启动；功能目录、共用资源和客机文件不增加启动。QEMU 内的 VM 创建/停止是检查内容，不等于重新启动 QEMU。未引用的文件不会因位于功能目录中就自动执行。

每个启动组的文件职责：

| 文件 | 作用 |
| --- | --- |
| `build-<target>[-variant].toml` | AxVisor构建配置及原`vm_configs`，文件命名保持原契约 |
| `qemu-<arch>[-variant].toml` | QEMU机器、启动参数、整组预算和有序检查引用 |
| `board-<selector>.toml` | 板型、启动与连接参数、整组预算和有序检查引用 |
| `tests/<用途>.toml` | 原检查命令、成功/失败条件、单项预算或探针声明 |
| `<功能>/*.py`等 | 原测试实现；跨组真实共用的实现只保留一份 |
| `guest.toml` | 单个客机的内核、vCPU、内存与设备，内部ID/name保持 |
| `guests/<角色>/guest.toml` | 多客机或多变体的配置；同类多个实例用不同角色目录 |
| `host-initramfs.toml` | 原客机包、BusyBox与固件准备输入，和客户机配置不同 |

测试自有的启动客机配置统一叫`guest.toml`。复用`os/axvisor/configs/vms`的生产配置保留原路径；动态生成的24客机配置及`sh/pool-*.toml`测试输入保持原文件名，避免改变原测试命令/断言。`starry-guest-build.toml`是构建配方，不改成客机配置名。

## 唯一执行入口

继续扫描原命名的QEMU/板卡运行配置，不引入全局`batches.toml`。组的配置显式引用检查：

```toml
[axbuild]
checks = ["tests/robot-virtual.toml", "tests/network.toml"]
```

场景名称取运行配置目录相对分组根的路径，例如`board/orangepi-5-plus/robot/robot-virtual-linux`；无需重复登记`id`。不再提供`aliases`兼容旧名称；测试名称取检查文件名（不含`.toml`）。工具仍支持外部调用方显式提供`id`；无外部检查的旧配置保持原发现规则。引用按书写顺序执行，相对当前运行配置解析；构建中的`vm_configs`等原项目路径语义不变。检查定义继续使用原`[[shell_check_steps]]`，或原`[host_http_probe]`。探针的`probe_script`和可选`asset_dir`相对功能定义解析：前者定位脚本，后者定位原夹具目录。

AxVisor适配层的`checks.rs`负责解析、选择与展开本系统的检查定义；共享`execution`层只提供串口、QEMU生命周期和截止时间执行能力，不识别`host_http_probe`等专用字段。外部QEMU检查组的纯串口、纯HTTP及混合执行统一保留成员边界，ostool继续负责QEMU进程与整组超时，不修改ostool。全部成员和前置完成、QEMU退出及串口排空均成功才通过。

每个成员从进入执行起只建立一次总截止时间，等待HTTP就绪、串口提示符、发送命令和探针结果都消耗同一预算，并受整组截止时间限制；步骤自己的上限只能进一步收紧。串口按接收时间判断成功标记，不接受迟到标记，也不误拒及时收到但晚处理的缓存。只有失败规则的命令步骤仍观察到返回提示符并检查错误。

同一次启动的HTTP成员必须使用相同的`guest_port`和`nic_model`，不兼容配置在启动前明确报错；脚本、夹具目录、环境及请求预算仍由各成员保留。串口socket以wait=on从开机捕获，HTTP期间及退出排空继续监控失败。有序成员执行当前需要Unix串口socket，其他宿主明确报不支持；无外部成员的传统纯串口配置仍沿用原执行器。自动检查使用`axvisor test`，手动Web交互使用`apps/suite/axvisor/qemu/aarch64`内的hostfwd配置，不把只启动QEMU当作测试通过。

## 如何看已登记测试

```sh
cargo xtask axvisor test qemu --arch aarch64 --list
cargo xtask axvisor test qemu --arch x86_64 --list
cargo xtask axvisor test board --list
cargo xtask axvisor app qemu --app-root apps/suite/axvisor --group qemu --arch aarch64 --list
```

`--list`表示可以发现的启动组，不表示进入CI或已经通过。自动范围以`.github/ci/checks/axvisor.toml`为准；夜间仍由`axvisor-nightly.toml`选择`apps/axvisor`，性能仍由`benchmarks.toml`选择原入口。

| normal QEMU | 启动组总数 | 主CI启动组 |
| --- | ---: | ---: |
| AArch64 | 7 | 6 |
| RISC-V | 1 | 1 |
| LoongArch | 1 | 1 |
| Intel VMX | 3 | 3 |
| AMD SVM | 2 | 2 |
| 合计 | 14 | 13 |

AArch64 `memory-web`以一次SMP1无盘启动承接原`qemu-host-initramfs`及`web-ui-diskless`两项检查；原90秒日志期限和180秒probe预算保留，整组270秒。`--test-case qemu/aarch64/memory-web`执行整组；追加`/memory-root`或`/dashboard`可单独选择其中一项，同时选择两项仍只启动一次。具体分组、单项命令和不能合并的边界见[qemu/README.md](qemu/README.md)。

AArch64 `ivc-local`与`ivc-arceos`分别以一次启动验证“源码 publisher + Linux subscriber”和“源码 publisher + 源码 subscriber”，各包含一项检查并进入主CI。当前30项普通功能检查，其中28项主CI检查由13个启动组承接。未进入主CI的QEMU检查仍是AArch64 `ivc`和RISC-V的`smoke/smp-ipi`；IVC混合客机仍保留原协议不匹配停用边界。manual的两个三ArceOS控制台变体保持手动。

板卡保留13个normal入口，其中8个进入主CI：OrangePi的三客机、两个单客机机器人、两个UART6双客机，以及ASUS、ROC、Phytium各一项。其余普通Linux/Starry启动、24客机网络、Rock4D、RDK-S100保持原未登记状态。两个perf30变体与virtcpufreq模板已整合到`apps/suite/axvisor/board/orangepi-5-plus/robot-linux-perf30`，不再由本目录发现，也未新增自动登记。`board --list`另兼容展示3个性能目录入口，不应将它们重复计入本目录CI。

## 使用和维护

```sh
cargo xtask axvisor test qemu --arch aarch64 --test-case qemu/aarch64/general/shell-and-block,qemu/aarch64/memory-web
cargo xtask axvisor test board --board orangepi-5-plus-triple-guest-functional --test-case board/orangepi-5-plus/triple-guest-functional
# 外部项目可以显式提供自己的套件根，包含所选分组，不要求TGOSKits目录布局：
cargo xtask axvisor test qemu --test-root /path/to/suite --test-group custom --arch aarch64 --list
```

新增测试先检查已有启动组。环境兼容且能保留全部检查、压力、初始状态和资源恢复时，追加功能定义与引用，复用启动；所需features可取兼容并集，不能使用`all-features`或破坏“某能力未启用”的负向条件。不同架构、VMX/SVM、GIC版本、根文件系统和互斥资源不能仅凭文件名相近就合并。

确需新启动时创建独立组，说明无法共用的具体条件。每次记录逻辑检查、组和实际启动数；合批做原独立/同启对照，以及首/中/末失败、超时和提前结束验证。未选中的检查不能因feature联动而执行。

删除/移动时更新运行引用、客户机/打包/夹具路径、CI选择及文档；共享文件先查所有消费者。不得以目录整理为理由删掉检查或降低负载。本轮不评估测试必要性或重叠步骤。

axbuild是独立宿主库，测试成员、目录策略和完成条件属于这些显式数据。不得将本树编译进工具，或让目标构建包含工具私有源码。改动后完成受影响行为验证，以及`python3 scripts/test/check_axbuild_independence.py`独立制品/外部消费者检查；生成配置和日志使用被忽略的输出目录。

## 公共资源归属

QEMU专用共享资源放`qemu/common`；跨板型共享放`board/common`；仅在QEMU与板卡实际共同使用时建立`common`。没有消费者就不创建空common。架构/板型专用入口留在对应功能目录，手动配置留`apps/suite/axvisor`。

当前`qemu/common/web-ui/http_probe.py`由ARM general及memory-web的dashboard检查执行，并被VMX web-ui入口导入。VMX入口位于`qemu/x86_64/vmx/web-ui/dashboard/default_guest_probe.py`，其asset_dir显式声明公共辅助目录，同时用于运行时导入和CI共享变更路由。手动demo配方与hostfwd位于`apps/suite/axvisor/qemu/aarch64/web-ui-demo`；API与三客机演示同属apps/suite，使用直接QEMU命令交互，不作为自动测试通过。详见[手动QEMU场景](../../apps/suite/axvisor/qemu/aarch64/README.md)。

### 仅声明文件的检查命名

只有检查定义文件时，使用`tests/<类别或功能>.toml`，例如robot-linux的`tests/robot-virtual.toml`。后续真实新增兼容网络检查时可增加`tests/network.toml`，并在board配置的checks中显式按顺序引用；不创建空占位项目。不必为每份TOML再套同名目录。脚本/多资产仍可保留在对应功能目录，检查定义统一放`tests/`并显式引用这些资产。`smoke`仅用于启动探针；机器人、网络、设备、性能检查按用途命名。

## 单项选择与共享启动

`--test-case`选择测试，可传检查文件名，也可用`<启动组路径>/<检查文件名>`限定；`<启动组路径>`选择整组，不传则执行所选环境下的全部检查。未知名称或短名对应多个运行配置时明确报错。板卡先用`--board`选择运行配置再匹配测试，QEMU先按架构/target匹配配置。名称取文件名，不额外登记id或别名。

```sh
# 原CI机器人：选择板端配置，执行其中一个实际测试
cargo xtask axvisor test board --board orangepi-5-plus-robot-linux \
  --test-case robot-virtual
# 合批后的两个检查分别诊断，或一起执行
cargo xtask axvisor test qemu --arch aarch64 \
  --test-case qemu/aarch64/memory-web/memory-root
cargo xtask axvisor test qemu --arch aarch64 \
  --test-case qemu/aarch64/memory-web/dashboard
cargo xtask axvisor test qemu --arch aarch64 \
  --test-case qemu/aarch64/memory-web/dashboard,qemu/aarch64/memory-web/memory-root
```

多项选择先合并到对应运行配置，再按`checks`登记顺序执行，重复选择不会增加执行或启动。命令参数中的先后顺序不改变登记顺序。`--list`展示启动组和可选择的成员；不是运行证明。

准备命令继续放在`tests/`中。单项需要公共准备时在检查文件中声明相对文件引用：

```toml
requires = ["guest-session.toml"]

[[shell_check_steps]]
shell_prefix = "orangepi@orangepi5plus:~"
shell_cmd = "执行本项检查"
success_regex = ["本项成功标记"]
```

`requires`先于当前文件执行，同一次启动中相同文件仅执行一次；递归循环、缺失文件和未知成员均失败。用于内部准备的文件不放入运行配置的`checks`，不作为独立功能登记。只选择benchmark不会自动执行机器人验证或客户机调频功能检查，但仍执行其显式引用的进入客户机/NPU性能配置等准备。没有公共前置的现有测试不强制拆文件，不创建`setup.toml`或新的setup配置层。

工具先选择再展开，未选中且不属于前置的检查不执行；任一已选检查或前置失败、超时均使整组失败。修改准备文件时CI路由追踪`requires`至实际消费者。旧`smoke-vmx`、`web-ui-diskless`等名称已移除，使用上面的场景路径与检查名称。

## OrangePi板卡与机器人分区

`board/orangepi-5-plus`保留标准板卡的Linux/Starry启动、三客户机功能和24客户机网络场景；`robot/`单独组织两个单客户机机器人和两个双客户机机器人，专属Zephyr共用配置归`robot/common/`。所有board配置文件名保持，场景路径增加robot这一层。

普通Linux启动场景通过`AXVISOR_GUEST_ASSETS`指定资产根，例如已准备好`/guest/linux/orangepi-5-plus`与initramfs的板卡可显式传`AXVISOR_GUEST_ASSETS=/guest`。该环境变量是资产输入，不应通过改全局配置修复缺失资产。

24客户机的标准board入口通过同目录`guest-builds.toml`自动生成配置并构建每份客户机；`--list`仍不运行准备。大规模网络检查仍为一次启动、24客户机、8核各3个vCPU和原48步，不因修复准备流程改变负载。

运行时宿主initramfs采用内容寻址路径，构建输出以`AXBUILD_GUEST_BUNDLE: `后的JSON发布实际路径与SHA256；跨进程消费者使用这份明确结果，不根据同架构固定文件名或目录mtime猜测最新产物。

OrangePi模拟机器人场景使用`robot/robot-virtual-linux`与`robot/robot-virtual-starry`目录名，`virtual`仅指执行器模拟。三客户机场景的配置直接放在场景根目录：`guest-linux.toml`、`guest-starry.toml`、`guest-zephyr.toml`；不再增加guests或角色子目录层。其他场景不因此强制改名。

原两项控制台检查interleave/atomic-output已迁入[apps/suite](../../apps/suite/axvisor/qemu/aarch64/console/README.md)，不再由test-suit发现；普通CI保留原25项功能，并接入新主线的PCI memory专项；general整组仍是原五项CI检查。

测试数据直接从本目录根发现，不再有normal物理分组，也无需传`--test-group normal`。仓库内场景仍以`qemu/...`和`board/...`选择；外部项目原有命名分组保持，平铺自定义根可显式使用`--test-group .`。应用的命名分组和SDK内部`test/normal`执行方式目录不受影响。

板卡后端或直接`axvisor qemu`入口只提供整组计时，不能静默忽略更短的成员预算：整组上限已足以约束全部成员时可执行，否则启动前拒绝该配置，要求使用有序QEMU测试/应用入口或保留独立限时板测场景。当前板卡配置仍保留原预算；未声明成员预算的旧应用继续使用其整组预算。
