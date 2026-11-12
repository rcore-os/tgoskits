# StarryOS系统测试

本目录采用normal测试入口：构建Starry内核，将用户态程序或脚本放入测试镜像，启动QEMU或板卡，再检查实际结果。CMake负责用户态程序的构建和安装；客户机runner负责逐项执行。目录按环境和功能组织，不另外维护suite清单。

## 目录与实现

| 位置 | 内容 |
| --- | --- |
| `qemu/standard/build-*.toml` | 四架构共享内核构建配置 |
| `qemu/standard/system/` | 根CMake、运行配置、功能分类及测试项目 |
| `system/{process,memory,fs,ipc,network,devices,perf,compat,ltp}/<项目>/` | 项目CMake和实现；分类不增加启动 |
| `qemu/standard/pid1*`、`host-initramfs*`等 | 依赖特殊开机/终止条件的独立case |
| `qemu/standard/openrc/` | 同一次启动检查TTY脚本、OpenRC服务、终端重拉起及关机停止；ARM还承接GICv2启动 |
| `qemu/x86_64/` | AHCI-only、单核抢占、E1000专用环境 |
| `board/<板型>/` | 构建、部署与运行配置；部分项目含C程序或脚本 |
| `common/{runner,cmake,include,network}/` | 真正共享的执行器、构建函数、头文件及网络辅助 |

每个C项目保留`CMakeLists.txt`与`src`，一个项目可以生成多个用户态程序。`system/CMakeLists.txt`递归收集功能分类下的项目，按原项目ID排序，一次configure/build/install；ID必须唯一。runner按原程序名执行，native先于LTP，每个程序独立PID/mount namespace并回收后代。

功能分类只决定源码位置；ARM的perf等启动分配仍由`system/qemu-aarch64.toml`中的`grouped_qemu_profiles`按项目ID前缀指定。放进`perf/`不等于自动进入icount环境。

宿主适配在`scripts/axbuild/src/test/normal/starry`，共用构建/注入/QEMU在`execution`。内核内部axtest仍由`os/StarryOS/kernel`和`cargo xtask ktest`负责，不能用宿主编译或用户态成功替代。

## 发现与选择

宿主寻找目标匹配的`build-<target>.toml`作为构建包装，再发现其运行`qemu-<arch>.toml`。`qemu/`总容器不放build文件；具体包装拥有构建配置。板测按`board-<selector>.toml`发现并解析最近构建配置。`.disabled`不自动执行。

公开测试ID保持稳定，和物理路径分离：

| 物理位置 | 选择ID |
| --- | --- |
| `qemu/standard/system` | `qemu/system` |
| `system/memory/syscall-test-brk` | `qemu/syscall-test-brk`或`qemu/system/syscall-test-brk` |
| `qemu/x86_64/ahci-single` | `ahci-single` |
| `qemu/x86_64/timer-preemption` | `timer-preemption-x86` |
| `qemu/x86_64/e1000/system` | `qemu-e1000/system` |
| `board/orangepi-5-plus/uvc-v4l2`（自带build） | `board-orangepi-5-plus/uvc-v4l2` |
| `board/orangepi-5-plus/robot-flow`（自带build） | `board-orangepi-5-plus/robot-flow` |

旧`qemu/tty-console-input-burst`选择转到OpenRC/TTY复合case；原TTY脚本仍执行，不另启一次内核。它主要是shell固定字符串/输出检查，不宣称验证真实突发串口输入。

ARM的旧`gicv2-boot`、`qemu/gicv2-boot`、`qemu/standard/gicv2-boot`选择运行完整OpenRC复合case，先检查原boot标记，再运行TTY/服务/终端/关机。ARM机器显式为`virt,gic-version=2`、A53、SMP4；这些旧选择在其他架构报错，无架构`--list`只列aarch64。普通OpenRC选择仍适用于四架构。

板测复用父build时保持`exec-cache`等原短ID；运行配置与build同目录时保持原带包装路径的长ID，同时接受新实体路径，不能自行缩短为叶目录名。

```bash
cargo xtask starry test qemu --arch riscv64
cargo xtask starry test qemu --arch aarch64 --list
cargo xtask starry test qemu --arch riscv64 -c qemu/system
cargo xtask starry test qemu --arch riscv64 -c qemu/syscall-test-brk
cargo xtask starry test qemu --arch x86_64 -c timer-preemption-x86
cargo xtask starry test board --board orangepi-5-plus --list
cargo xtask starry test board --board orangepi-5-plus -c exec-cache,pwm-sysfs --server <host> --port <port>
```

当前默认QEMU预算：RISC-V 6、AArch64 14、x86_64 10、LoongArch64 6，共36次；CI另外每架构一次starry-kernel ktest，共4次。ARM system包括普通、perf icount、根扩容、重挂载及两次额外FAT根启动，共6次。GICv2 boot已并入ARM OpenRC，不再额外启动。没有把list行数、程序数或构建次数当启动数；失败/重试/条件跳过另计。

## 板端共享启动

板测先过滤缺少`requirements.toml`环境变量的成员。只有声明`shared_boot = true`，或由`shared_boot_selectors = ["selector"]`明确选中的运行变体，才尝试按真实Cargo构建身份、板型、cmdline、DTB、装载地址及服务参数归并启动。声明前必须验证命令会归还shell、资源状态允许后续检查；未声明默认独立。同目录有不同物理流程时，优先按selector声明，避免把未验证的变体一起启用。

同批每项仍保留原命令、成功/失败规则和超时，按原选择的相对顺序执行；批次按首个成员出现顺序执行。上传文件相对路径保持，重名且内容不同在占板前报错；只在完整批次完成后报告成员通过。前项错误不能让后项或末尾标记冒充整批成功。

DTB优先使用测试环境预设：普通OrangePi五项全部省略`dtb_file`，由板卡服务提供当前会话DTB，共启动一次。需要验证指定设备树时才显式设置；缺省与显式DTB仍不自动视为等价，不能放宽合同比较来强行归批。

当前两个OrangePi选择器分别运行一次：

```text
orangepi-5-plus             原单核构建，环境预设DTB
└── exec-cache → native-hardware-smoke → native-network-smoke → pwm-sysfs → rknpu-resources
orangepi-5-plus-robot       原CPU上限8的构建，环境预设DTB
└── robot-flow（保留原四步）→ uvc-v4l2
```

纯单步批次保留成员命令预算，整批预算相加。多步骤成员只能排在首位，必须有明确总超时、每步命令和末步成功判定；后续成员必须为单步且其预算不大于首成员预算。整批沿用首成员总上限，保留其原步骤超时，不在每步重新发放总预算；后项同时受自己的命令上限和整批剩余时间约束。这是保守共用上限，不能把两个多步骤流程直接拼接。虚拟批次因此共用900秒，主程序300秒、UVC180秒不变；手动单选仍沿用各自原配置。

AKA-00-SG2002在原无线条件齐备时，五项按`boot → tennis-yolo → usb2-lsusb → vdec → wifi-network-smoke`共享一次启动，默认从三批减为一批。保留原SMP1构建、features、运行命令、3000秒整批预算及每项600秒命令上限；先完成原requirements与构建校验，缺失已声明环境变量的成员明确skip，不计通过，也不进入批次。仅选择VDEC和Wi-Fi时，两项同启一次；单选仍可独立诊断。

无线凭据沿用原环境变量注入，不能写入仓库。安全Wi-Fi启动仍用原AKA设备树生成本次会话的熵，每次实际启动仅生成一次；本次合并不改设备树策略。真实机器人nightly不启用共享启动，纯预装程序批次允许没有上传文件。

机器人selector和真实board_type/布线资源分开看，不因名字相同自动共享启动。人工测试显式指定服务端，运行后确认自己的会话释放，不抢占其他任务。

## CI范围与未自动执行

- 主CI四架构QEMU及AKA、VisionFive2、JL在`.github/ci/checks/starry.toml`；普通OrangePi和robot selector在`axvisor.toml`。
- 真实robot工作流在`axvisor-nightly.toml`；Apps和benchmark由各自清单/入口负责。
- LicheeRV Nano两份运行配置、Rock4D一份目前未注册主CI/nightly，不因搬目录自动启用。
- AKA `usb2-libuvc-init`保留disabled模板；旧注释中的网络限制是历史描述，不据此认定当前网络必然不可用。
- system仍有5项安装到`starry-known-fail`，默认不执行；其余部分程序可能因架构/设备不适用输出skip。编译、被发现与有效功能通过必须分开记录。

## 添加、修改、删除

压力循环的次数上限、每次数据量、并发条件和阻塞方式属于测试内容，不能通过削减负载宣称等价提速。socket数据面SM8保留最多200次、每次4096字节的阻塞发送，仅遇发送错误时按原逻辑结束循环；循环后汇总尝试次数、成功返回的累计字节数、最终返回值和错误码。非空发送的成功返回必须为合法正字节数，终止错误只接受EPIPE、ECONNRESET或ENOTCONN，其他结果计入失败并传播至外层。SIGPIPE仍被忽略作为保护，该项不宣称验证MSG_NOSIGNAL的信号抑制效果。

1. 先检查已有项目能否承接同一功能和错误信号。C项目放对应功能分类，保留唯一ID、CMake与实现；不按历史bugfix来源增加新分类。
2. 兼容程序优先加入现system启动；新增feature优先在兼容性验证后加入共享build配置，不仅因feature不同就新增启动。只有具体设备、特权/根启动、单核竞争、icount或预期终止条件不能共存时才新增case。
3. 改目录同步CMake相对引用、单项选择、CI路径路由和文档。旧路径删除也须路由到当前功能，不靠保留重复源码作兼容。
4. 使用现`--list`核对发现和选择，再运行受影响单项及所属组合；保留LTP的版本、TPASS/文件系统完成数与真实错误退出判定。
5. 合批实跑原单项、组合、代表性错误与恢复，确认外层非零及后项未误判。删除函数只算负载减少，取消独立启动才算启动减少。
6. 不通过扩大正则、吞异常、无条件skip或延长功能内部时限制造通过。PID1终止、设备缺失与物理只读等真实前提保留。

详细机制和环境要求见[GUIDE](GUIDE.md)。
